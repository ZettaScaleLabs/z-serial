//
// Copyright (c) 2022 ZettaScale Technology
//
// This program and the accompanying materials are made available under the
// terms of the Eclipse Public License 2.0 which is available at
// http://www.eclipse.org/legal/epl-2.0, or the Apache License, Version 2.0
// which is available at https://www.apache.org/licenses/LICENSE-2.0.
//
// SPDX-License-Identifier: EPL-2.0 OR Apache-2.0
//
// Contributors:
//   ZettaScale Zenoh Team, <zenoh@zettascale.tech>
//

use std::{path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{ClearBuffer, SerialPort, SerialPortBuilderExt, SerialStream};

pub const MAX_FRAME_SIZE: usize = 1510;
pub const MAX_MTU: usize = 1500;

const CRC32_LEN: usize = 4;

// Given by cobs::max_encoding_length(MAX_FRAME_SIZE)
const COBS_BUF_SIZE: usize = 1517;

const LEN_FIELD_LEN: usize = 2;

const KIND_FIELD_LEN: usize = 1;

const SENTINEL: u8 = 0x00;

const CRC_TABLE_SIZE: usize = 256;

const POLYNOMIA: u32 = 0x04C11DB7;

const SERIAL_CONNECT_THROTTLE_TIME_US: u64 = 250_000;

/// ZSerial Frame Format
///
/// Using COBS
///
/// +-+-+----+------------+--------+-+
/// |O|H|XXXX|ZZZZ....ZZZZ|CCCCCCCC|0|
/// +-+----+------------+--------+-+
/// |O| |Len |   Data     |  CRC32 |C|
/// +-+-+-2--+----N-------+---4----+-+
///
/// Header: 1byte
/// +---------------+
/// |7|6|5|4|3|2|1|0|
/// +---------------+
/// |x|x|x|x|x|R|A|I|
/// +---------------+
///
/// Flags:
/// I - Init
/// A - Ack
/// R - Reset
///
/// Max Frame Size: 1510
/// Max MTU: 1500
/// Max On-the-wire length: 1516 (MFS + Overhead Byte (OHB) + Kind Byte + End of packet (EOP))

/// Initializaiton message exchange from cold start
///
/// 1) Connect side sends a message with empty payload and I flag set.
/// 2) Listen side sends a message with empty payload and I+A flags set.
/// 3) Data can be exchanged from this point on.
///
///  ┌─────────┐                ┌──────────┐   
///  │ Listen  │                │ Connect  │   
///  └────┬────┘                └────┬─────┘   
///       │                          │         
///       │       Init               │         
///       │◄─────────────────────────┤         
///       │                          │         
///       │        Init + Ack        │         
///       ├─────────────────────────►│         
///       │       Data               │         
///       │◄─────────────────────────┤         
///       ├─────────────────────────►│         
///       │                          │         
///
/// If connect sides restarts:
///
/// 1) Connect side sends a message with empty payload and I flag set.   
/// 2) Listen side sends a message with empty payload and R flag set.
/// 3) Connect side waits and goes back to cold start case.
///                                                                                                
///   ┌─────────┐                ┌──────────┐
///   │ Listen  │                │ Connect  │
///   └────┬────┘                └────┬─────┘
///        │        Init              │       
///        │◄─────────────────────────┤       
///        │       Reset              │       
///        ├─────────────────────────►│       
///        │                          │       
///        │                          │       
///        │                          │       
///        │        Init              │       
///        │◄─────────────────────────┤       
///        │       Init + Ack         │       
///        ├─────────────────────────►│       
///        │         Data             │       
///        │◄─────────────────────────┤       
///        ├─────────────────────────►│       
///        │                          │       
///        │                          │       

const I_FLAG: u8 = 0x01;
const A_FLAG: u8 = 0x02;
const R_FLAG: u8 = 0x04;

#[derive(Debug)]
struct Header(u8);

impl Header {
    pub fn new(flags: u8) -> Self {
        Header(flags)
    }

    pub fn has_i_flag(&self) -> bool {
        self.0 & I_FLAG == I_FLAG
    }

    pub fn has_a_flag(&self) -> bool {
        self.0 & A_FLAG == A_FLAG
    }

    pub fn has_r_flag(&self) -> bool {
        self.0 & R_FLAG == R_FLAG
    }

    pub fn get_byte(&self) -> u8 {
        self.0
    }
}

impl From<u8> for Header {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

#[derive(Debug)]
pub struct CRC32 {
    table: [u32; CRC_TABLE_SIZE],
}

impl CRC32 {
    pub fn compute_crc32(&self, buff: &[u8]) -> u32 {
        let mut acc: u32 = !0;

        for b in buff {
            let octect = *b;
            acc = (acc >> 8) ^ self.table[((acc & 0xFF) ^ octect as u32) as usize]
        }

        !acc
    }
}

impl Default for CRC32 {
    fn default() -> Self {
        let mut table = [0u32; CRC_TABLE_SIZE];
        for n in 0..256 {
            let mut rem = n;

            for _ in 0..8 {
                match rem & 1 {
                    1 => rem = POLYNOMIA ^ (rem >> 1),
                    _ => rem >>= 1,
                }
            }

            table[n as usize] = rem;
        }

        Self { table }
    }
}

#[derive(Debug)]
struct WireFormat {
    buff: Vec<u8>,
    crc: CRC32,
}

impl WireFormat {
    pub(crate) fn new() -> Self {
        // Generating CRC table
        let crc = CRC32::default();

        Self {
            buff: vec![0u8; COBS_BUF_SIZE],
            crc,
        }
    }

    pub(crate) fn serialize_into(
        &mut self,
        src: &[u8],
        dest: &mut [u8],
        header: Header,
    ) -> tokio_serial::Result<usize> {
        if src.len() > MAX_MTU {
            return Err(tokio_serial::Error::new(
                tokio_serial::ErrorKind::InvalidInput,
                "Payload is too big",
            ));
        }

        // Compute CRC
        let crc32 = self.crc.compute_crc32(src).to_ne_bytes();

        // Compute wise_size
        let wire_size: u16 = src.len() as u16;

        let size_bytes = wire_size.to_ne_bytes();

        // Copy into serialization buffer
        self.buff[0] = header.get_byte();
        self.buff[KIND_FIELD_LEN..LEN_FIELD_LEN + KIND_FIELD_LEN].copy_from_slice(&size_bytes);
        self.buff[LEN_FIELD_LEN + KIND_FIELD_LEN..LEN_FIELD_LEN + KIND_FIELD_LEN + src.len()]
            .copy_from_slice(src);
        self.buff[LEN_FIELD_LEN + KIND_FIELD_LEN + src.len()
            ..LEN_FIELD_LEN + KIND_FIELD_LEN + src.len() + CRC32_LEN]
            .copy_from_slice(&crc32);

        let total_len = KIND_FIELD_LEN + LEN_FIELD_LEN + CRC32_LEN + src.len();

        log::trace!(
            "Frame before COBS encoding {:02X?}",
            &self.buff[0..total_len]
        );

        // COBS encode
        let mut written = cobs::encode_with_sentinel(&self.buff[0..total_len], dest, SENTINEL);

        // Add sentinel byte, marks the end of a message
        dest[written] = SENTINEL;
        written += 1;

        Ok(written)
    }

    pub(crate) fn deserialize_into(
        &self,
        src: &mut [u8],
        dst: &mut [u8],
    ) -> tokio_serial::Result<(usize, Header)> {
        let decoded_size = cobs::decode_in_place_with_sentinel(src, SENTINEL).map_err(|e| {
            tokio_serial::Error::new(
                tokio_serial::ErrorKind::InvalidInput,
                format!("Unable COBS decode: {e:?}"),
            )
        })?;

        log::trace!("Frame after COBS encoding {:02X?}", &src[0..decoded_size]);

        // Check if message has the minimum size
        if decoded_size < LEN_FIELD_LEN + CRC32_LEN {
            return Err(tokio_serial::Error::new(
                tokio_serial::ErrorKind::InvalidInput,
                "Serial is smaller than the minimum size",
            ));
        }

        // Decoding message kind
        let hdr = Header::from(src[0]);
        // Decoding message size
        let wire_size = ((src[2] as u16) << 8 | src[1] as u16) as usize;

        // Check if the frame size is correct
        if KIND_FIELD_LEN + LEN_FIELD_LEN + wire_size + CRC32_LEN != decoded_size {
            return Err(tokio_serial::Error::new(
                tokio_serial::ErrorKind::InvalidInput,
                "Payload does not match the its size",
            ));
        }

        // Getting the data
        let data = &src[KIND_FIELD_LEN + LEN_FIELD_LEN..KIND_FIELD_LEN + wire_size + LEN_FIELD_LEN];

        let crc_received_bytes = &src[KIND_FIELD_LEN + LEN_FIELD_LEN + wire_size
            ..KIND_FIELD_LEN + LEN_FIELD_LEN + wire_size + CRC32_LEN];

        let recv_crc: u32 = ((crc_received_bytes[3] as u32) << 24)
            | ((crc_received_bytes[2] as u32) << 16)
            | ((crc_received_bytes[1] as u32) << 8)
            | (crc_received_bytes[0] as u32);

        // Compute CRC locally
        let computed_crc = self.crc.compute_crc32(&data[0..wire_size]);

        log::trace!("Received CRC {recv_crc:02X?}  Computed CRC {computed_crc:02X?}");

        // Check CRC
        if recv_crc != computed_crc {
            return Err(tokio_serial::Error::new(
                tokio_serial::ErrorKind::InvalidInput,
                format!(
                    "CRC does not match Received {:02X?} Computed {:02X?}",
                    recv_crc, computed_crc
                ),
            ));
        }

        // Copy into user slice.
        dst[0..wire_size].copy_from_slice(data);

        Ok((wire_size, hdr))
    }
}

#[derive(PartialEq, Eq)]
enum Status {
    Uninitialized,
    Initialized,
}

pub struct ZSerial {
    port: String,
    baud_rate: u32,
    serial: SerialStream,
    send_buff: Vec<u8>,
    recv_buff: Vec<u8>,
    formatter: WireFormat,
    status: Status,
}

impl ZSerial {
    pub fn new(port: String, baud_rate: u32, exclusive: bool) -> tokio_serial::Result<Self> {
        let mut serial = tokio_serial::new(port.clone(), baud_rate).open_native_async()?;

        #[cfg(unix)]
        serial.set_exclusive(exclusive)?;
        serial.clear(ClearBuffer::All)?;

        Ok(Self {
            port,
            baud_rate,
            serial,
            send_buff: vec![0u8; COBS_BUF_SIZE],
            recv_buff: vec![0u8; COBS_BUF_SIZE],
            formatter: WireFormat::new(),
            status: Status::Uninitialized,
        })
    }

    pub fn close(&mut self) {
        self.status = Status::Uninitialized;
    }

    pub async fn accept(&mut self) -> tokio_serial::Result<()> {
        if self.status == Status::Initialized {
            return Err(tokio_serial::Error {
                kind: tokio_serial::ErrorKind::InvalidInput,
                description: "Cannot accept on an intialized connection!".into(),
            });
        }

        log::trace!("Waiting for connection");
        let mut buff = vec![0u8; COBS_BUF_SIZE];

        // Clear all buffers
        self.clear()?;

        loop {
            // wait for an empty message with I flag

            let (_read, hdr) = self.internal_read(&mut buff).await?;
            log::trace!("Received header: {hdr:02X?}");
            if hdr.has_i_flag() {
                // we send back a message with both I and A flags
                self.internal_write(&[0u8], Header::new(I_FLAG | A_FLAG))
                    .await?;

                // we must set our internal status to initialized
                self.status = Status::Initialized;
                return Ok(());
            } // otherwise ignore
        }
    }

    pub async fn connect(&mut self, tout: Option<Duration>) -> tokio_serial::Result<()> {
        let tout = tout.unwrap_or(Duration::from_micros(SERIAL_CONNECT_THROTTLE_TIME_US));
        let mut buff = vec![0u8; COBS_BUF_SIZE];

        // we must first send a en emtpy message with I flag
        loop {
            let hdr = Header::new(I_FLAG);
            log::trace!("Sending {hdr:02X?}");
            self.internal_write(&[0], hdr).await?;

            // we then wait for a message with both I and A flags
            let (_read, hdr) = self.internal_read(&mut buff).await?;
            log::trace!("Received header: {hdr:02X?}");

            if hdr.has_a_flag() && hdr.has_i_flag() {
                // correct initialiation
                self.status = Status::Initialized;
                break;
            } else if hdr.has_r_flag() {
                // we received a reset we must resend the init message after a small sleep
                tokio::time::sleep(tout).await;
                continue;
            } else {
                return Err(tokio_serial::Error {
                    kind: tokio_serial::ErrorKind::InvalidInput,
                    description: format!("Unknown header: {hdr:02X?}"),
                });
            }
        }

        Ok(())
    }

    pub async fn dump(&mut self) -> tokio_serial::Result<()> {
        self.serial
            .read_exact(std::slice::from_mut(&mut self.recv_buff[0]))
            .await?;
        println!("Read {:02X?}", self.recv_buff[0]);
        Ok(())
    }

    async fn internal_read(&mut self, buff: &mut [u8]) -> tokio_serial::Result<(usize, Header)> {
        let mut start_count = 0;

        if buff.len() < MAX_MTU {
            return Err(tokio_serial::Error::new(
                tokio_serial::ErrorKind::InvalidInput,
                format!("Recv buffer is too small, required minimum {MAX_MTU}"),
            ));
        }

        // Read
        loop {
            // Check if we are reading too much, maybe we lost the sentinel.
            if start_count == COBS_BUF_SIZE {
                return Ok((0, Header::new(0u8)));
            }

            // Read one byte at time until we reach the sentinel
            self.serial
                .read_exact(std::slice::from_mut(&mut self.recv_buff[start_count]))
                .await?;

            if self.recv_buff[start_count] == SENTINEL {
                break;
            }
            start_count += 1;
        }

        start_count += 1;

        log::trace!(
            "Read {start_count} bytes COBS {:02X?}",
            &self.recv_buff[0..start_count]
        );

        // Deserialize
        self.formatter
            .deserialize_into(&mut self.recv_buff[0..start_count], buff)
    }

    pub async fn read_msg(&mut self, buff: &mut [u8]) -> tokio_serial::Result<usize> {
        let (read, hdr) = self.internal_read(buff).await?;
        //TODO: check the kind is the expected one.
        if self.status == Status::Initialized && hdr.has_i_flag() {
            // we must rest the connection here
            self.internal_write(&[0u8], Header::new(R_FLAG)).await?;
            self.status = Status::Uninitialized;
            return Err(tokio_serial::Error {
                kind: tokio_serial::ErrorKind::InvalidInput,
                description: "Unexpected Init flag in message".into(),
            });
        }

        Ok(read)
    }

    #[allow(dead_code)]
    async fn read(serial: &mut SerialStream, buff: &mut [u8]) -> tokio_serial::Result<usize> {
        Ok(serial.read(buff).await?)
    }

    #[allow(dead_code)]
    async fn read_all(serial: &mut SerialStream, buff: &mut [u8]) -> tokio_serial::Result<()> {
        let mut read: usize = 0;
        while read < buff.len() {
            let n = Self::read(serial, &mut buff[read..]).await?;
            read += n;
        }
        Ok(())
    }

    async fn internal_write(&mut self, buff: &[u8], hdr: Header) -> tokio_serial::Result<()> {
        // Serialize
        let written = self
            .formatter
            .serialize_into(buff, &mut self.send_buff, hdr)?;

        log::trace!(
            "Wrote {written}bytes COBS {:02X?}",
            &self.send_buff[0..written]
        );

        // Write
        self.serial.write_all(&self.send_buff[0..written]).await?;
        self.serial.flush().await?;
        Ok(())
    }

    pub async fn write(&mut self, buff: &[u8]) -> tokio_serial::Result<()> {
        self.internal_write(buff, Header::new(0u8)).await
    }

    /// Gets the configured baud rate
    pub fn baud_rate(&self) -> u32 {
        self.baud_rate
    }

    /// Gets the configured serial port
    pub fn port(&self) -> String {
        self.port.clone()
    }

    pub fn bytes_to_read(&self) -> tokio_serial::Result<u32> {
        self.serial.bytes_to_read()
    }

    pub fn clear(&self) -> tokio_serial::Result<()> {
        self.serial.clear(ClearBuffer::All)
    }
}

pub fn get_available_port_names() -> tokio_serial::Result<Vec<String>> {
    let port_names: Vec<String> = tokio_serial::available_ports()?
        .iter()
        .map(|info| {
            Path::new(&info.port_name)
                .file_name()
                .map(|os_str| os_str.to_string_lossy().to_string())
                .ok_or_else(|| {
                    tokio_serial::Error::new(
                        tokio_serial::ErrorKind::Unknown,
                        "Unsupported port name",
                    )
                })
        })
        .collect::<Result<Vec<String>, _>>()?;

    Ok(port_names)
}

#[cfg(test)]
mod tests {
    use crate::Header;

    use super::{WireFormat, COBS_BUF_SIZE};

    #[test]
    fn test_ser() {
        let mut formatter = WireFormat::new();
        let mut ser_buff = vec![0u8; COBS_BUF_SIZE];

        let data: Vec<u8> = vec![0x00, 0x11, 0x00];

        // COBS encoded 0x00 | 0x03 0x00 | 0x00 0x11 0x00 | 0x73 0xEC 0x75 0xF9 |
        //              Hdr |   Len     |   Data         |      CRC32          |
        let serialzed_data: Vec<u8> = vec![
            0x01, 0x02, 0x03, 0x01, 0x02, 0x11, 0x05, 0x73, 0xEC, 0x75, 0xF9, 0x00,
        ];

        // Checks serialization
        let written = formatter
            .serialize_into(&data, &mut ser_buff, Header::new(0u8))
            .unwrap();
        assert_eq!(written, serialzed_data.len());
        assert_eq!(serialzed_data, ser_buff[0..written]);

        //2nd Check

        let data: Vec<u8> = vec![0x11, 0x22, 0x00, 0x33];

        // COBS encoded 0x00 | 0x04 0x00 | 0x11 0x22 0x00 0x33 | 0x8D 0x03 0x6D 0xFB |
        //              Hdr |   Len     |   Data              |      CRC32          |
        let serialzed_data: Vec<u8> = vec![
            0x01, 0x02, 0x04, 0x03, 0x11, 0x22, 0x06, 0x33, 0x8D, 0x03, 0x6D, 0xFB, 0x00,
        ];

        let written = formatter
            .serialize_into(&data, &mut ser_buff, Header::new(0u8))
            .unwrap();
        assert_eq!(written, serialzed_data.len());
        assert_eq!(serialzed_data, ser_buff[0..written]);
    }

    #[test]
    fn test_de() {
        let formatter = WireFormat::new();
        let mut buff = vec![0u8; COBS_BUF_SIZE];

        let data: Vec<u8> = vec![0x00, 0x11, 0x00];
        // COBS encoded 0x01 | 0x03 0x00 | 0x00 0x11 0x00 | 0x73 0xEC 0x75 0xF9 |
        //              Hdr |   Len     |   Data         |      CRC32          |
        let mut serialzed_data: Vec<u8> = vec![
            0x01, 0x02, 0x03, 0x01, 0x02, 0x11, 0x05, 0x73, 0xEC, 0x75, 0xF9, 0x00,
        ];
        let serialized_len = serialzed_data.len();

        let (read, hdr) = formatter
            .deserialize_into(&mut serialzed_data[0..serialized_len], &mut buff)
            .unwrap();

        assert_eq!(read, data.len());
        assert!(!hdr.has_i_flag());
        assert_eq!(buff[0..read], data);

        //2nd Check

        let data: Vec<u8> = vec![0x11, 0x22, 0x00, 0x33];

        // COBS encoded 0x02 | 0x04 0x00 | 0x11 0x22 0x00 0x33 | 0x8D 0x03 0x6D 0xFB |
        //              hdr |   Len     |   Data              |      CRC32          |
        let mut serialzed_data: Vec<u8> = vec![
            0x01, 0x02, 0x04, 0x03, 0x11, 0x22, 0x06, 0x33, 0x8D, 0x03, 0x6D, 0xFB, 0x00,
        ];
        let serialized_len = serialzed_data.len();

        let (read, hdr) = formatter
            .deserialize_into(&mut serialzed_data[0..serialized_len], &mut buff)
            .unwrap();

        assert_eq!(read, data.len());
        assert_eq!(buff[0..read], data);
        assert!(!hdr.has_i_flag());
    }

    #[test]
    fn test_serde_one_byte() {
        let mut formatter = WireFormat::new();
        let mut ser_buff = vec![0u8; COBS_BUF_SIZE];
        let mut de_buff = vec![0u8; COBS_BUF_SIZE];

        let data: Vec<u8> = vec![0x00];
        let written = formatter
            .serialize_into(&data, &mut ser_buff, Header::new(0u8))
            .unwrap();

        println!("Data: {data:02X?}");
        println!("Serialized: {:02X?}", &ser_buff[0..written]);

        let (read, hdr) = formatter
            .deserialize_into(&mut ser_buff[0..written], &mut de_buff)
            .unwrap();

        println!("Deserialized: {:02X?}", &de_buff[0..read]);

        assert_eq!(read, data.len());
        assert!(!hdr.has_i_flag());

        assert_eq!(data, de_buff[0..read]);
    }

    #[test]
    fn test_serde_emtpy() {
        let mut formatter = WireFormat::new();
        let mut ser_buff = vec![0u8; COBS_BUF_SIZE];
        let mut de_buff = vec![0u8; COBS_BUF_SIZE];

        let data: Vec<u8> = vec![];
        let written = formatter
            .serialize_into(&data, &mut ser_buff, Header::new(0u8))
            .unwrap();

        println!("Data: {data:02X?}");
        println!("Serialized: {:02X?}", &ser_buff[0..written]);

        let (read, hdr) = formatter
            .deserialize_into(&mut ser_buff[0..written], &mut de_buff)
            .unwrap();

        println!("Deserialized: {:02X?}", &de_buff[0..read]);

        assert_eq!(read, data.len());
        assert!(!hdr.has_i_flag());

        assert_eq!(data, de_buff[0..read]);
    }
}
