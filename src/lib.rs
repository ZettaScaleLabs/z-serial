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

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{ClearBuffer, SerialPort, SerialPortBuilderExt, SerialStream};

pub const MAX_FRAME_SIZE: usize = 1510;
pub const MAX_MTU: usize = 1500;

const CRC32_LEN: usize = 4;

const SERIAL_BUF_SIZE: usize = 1500;

// Given by cobs::max_encoding_length(MAX_FRAME_SIZE)
const COBS_BUF_SIZE: usize = 1516;

const LEN_FIELD_LEN: usize = 2;

const SENTINEL: u8 = 0x00;

const CRC_TABLE_SIZE: usize = 256;

const POLYNOMIA: u32 = 0x04C11DB7;

/// ZSerial Frame Format
///
/// Using COBS
///
/// +----+------------+--------+-+
/// |XXXX|ZZZZ....ZZZZ|CCCCCCCC|0|
/// +----+------------+--------+-+
/// | Len|   Data     |  CRC32 |C|
/// +-2--+----N-------+---4----+-+
///
/// Max Frame Size: 1510
/// Max MTU: 1500

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

pub struct ZSerial {
    port: String,
    baud_rate: u32,
    serial: SerialStream,
    buff: Vec<u8>,     //[u8; SERIAL_BUF_SIZE],
    ser_buff: Vec<u8>, //[u8; COBS_BUF_SIZE],
    crc: CRC32,
}

impl ZSerial {
    pub fn new(port: String, baud_rate: u32) -> tokio_serial::Result<Self> {
        // Generating CRC table
        let crc = CRC32::default();

        let mut serial = tokio_serial::new(port.clone(), baud_rate).open_native_async()?;

        #[cfg(unix)]
        serial.set_exclusive(false)?;

        Ok(Self {
            port,
            baud_rate,
            serial,
            buff: vec![0u8; SERIAL_BUF_SIZE],
            ser_buff: vec![0u8; COBS_BUF_SIZE],
            crc,
        })
    }

    pub async fn dump(&mut self) -> tokio_serial::Result<()> {
        self.serial
            .read_exact(std::slice::from_mut(&mut self.buff[0]))
            .await?;
        println!("Read {:02X?}", self.buff[0]);
        Ok(())
    }

    pub async fn read_msg(&mut self, buff: &mut [u8]) -> tokio_serial::Result<usize> {
        let mut start_count = 0;

        if buff.len() < MAX_MTU {
            return Err(tokio_serial::Error::new(
                tokio_serial::ErrorKind::InvalidInput,
                format!("Recv buffer is too small, required minimum {MAX_MTU}"),
            ));
        }

        loop {
            // Check if we are reading too much, maybe we lost the sentinel.
            if start_count == COBS_BUF_SIZE {
                return Ok(0);
            }

            // Read one byte at time until we reach the sentinel
            self.serial
                .read_exact(std::slice::from_mut(&mut self.ser_buff[start_count]))
                .await?;

            if self.ser_buff[start_count] == SENTINEL {
                break;
            }
            start_count += 1;
        }
        let _size =
            cobs::decode_in_place_with_sentinel(&mut self.ser_buff[0..start_count], SENTINEL)
                .map_err(|e| {
                    tokio_serial::Error::new(
                        tokio_serial::ErrorKind::InvalidInput,
                        format!("Unable COBS decode: {e:?}"),
                    )
                })?;

        // Decoding message size
        let wire_size = ((self.ser_buff[1] as u16) << 8 | self.ser_buff[0] as u16) as usize;

        // Getting the data
        let data = &self.ser_buff[LEN_FIELD_LEN..wire_size + LEN_FIELD_LEN];

        // Getting and decoding CRC
        let crc_received_bytes =
            &self.ser_buff[LEN_FIELD_LEN + wire_size..LEN_FIELD_LEN + wire_size + CRC32_LEN];

        let recv_crc: u32 = ((crc_received_bytes[3] as u32) << 24)
            | ((crc_received_bytes[2] as u32) << 16)
            | ((crc_received_bytes[1] as u32) << 8)
            | (crc_received_bytes[0] as u32);

        // Compute CRC locally
        let computed_crc = self.crc.compute_crc32(&data[0..wire_size]);

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
        buff[0..wire_size].copy_from_slice(data);

        return Ok(wire_size);
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

    pub async fn write(&mut self, buff: &[u8]) -> tokio_serial::Result<()> {
        if buff.len() > MAX_MTU {
            return Err(tokio_serial::Error::new(
                tokio_serial::ErrorKind::InvalidInput,
                "Payload is too big",
            ));
        }

        // Compute crc
        let crc32 = self.crc.compute_crc32(buff).to_ne_bytes();

        // Compute wise_size
        let wire_size: u16 = buff.len() as u16;

        let size_bytes = wire_size.to_ne_bytes();

        // Copy into serialization buffer
        self.buff[0..LEN_FIELD_LEN].copy_from_slice(&size_bytes);
        self.buff[LEN_FIELD_LEN..LEN_FIELD_LEN + buff.len()].copy_from_slice(&buff);
        self.buff[LEN_FIELD_LEN + buff.len()..LEN_FIELD_LEN + buff.len() + CRC32_LEN]
            .copy_from_slice(&crc32);

        let total_len = LEN_FIELD_LEN + CRC32_LEN + buff.len();

        // COBS encode
        let written =
            cobs::encode_with_sentinel(&self.buff[0..total_len], &mut self.ser_buff, SENTINEL);
        // Add sentinel byte, marks the end of a message
        self.ser_buff[written + 1] = SENTINEL;

        // Write over serial
        self.serial
            .write_all(&self.ser_buff[0..written + 1])
            .await?;

        Ok(())
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
