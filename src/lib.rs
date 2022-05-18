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
const CRC32_LEN: usize = 4;

const LEN_FIELD_LEN: usize = 2;

const PREAMBLE: [u8; 4] = [0xF0, 0x0F, 0x0F, 0xF0];

const MAX_MTU: usize = 1500;

const CRC_TABLE_SIZE: usize = 256;

const POLYNOMIA: u32 = 0x04C11DB7;

/// ZSerial Frame Format
///
///
/// +--------+----+------------+--------+
/// |F00F0FF0|XXXX|ZZZZ....ZZZZ|CCCCCCCC|
/// +--------+----+------------+--------+
/// |Preamble| Len|   Data     |  CRC32 |
/// +----4---+-2--+----N-------+---4----+
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
    buff: [u8; MAX_FRAME_SIZE],
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
            buff: [0u8; MAX_FRAME_SIZE],
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
            // Wait for sync preamble: 0xF0 0x0F 0x0F 0xF0

            // Read one byte

            self.serial
                .read_exact(std::slice::from_mut(&mut self.buff[start_count]))
                .await?;

            if start_count == 0 {
                if self.buff[start_count] == PREAMBLE[0] {
                    // First sync byte found
                    start_count = 1;
                }
            } else if start_count == 1 {
                if self.buff[start_count] == PREAMBLE[1] {
                    // Second sync byte found
                    start_count = 2;
                }
            } else if start_count == 2 {
                if self.buff[start_count] == PREAMBLE[2] {
                    // Third sync byte found
                    start_count = 3;
                }
            } else if start_count == 3 {
                if self.buff[start_count] == PREAMBLE[3] {
                    // fourth and last sync byte found
                    start_count = 4;

                    // lets read the len now
                    self.serial
                        .read_exact(&mut self.buff[start_count..start_count + LEN_FIELD_LEN])
                        .await?;

                    let size: usize = ((self.buff[start_count + 1] as u16) << 8
                        | self.buff[start_count] as u16)
                        as usize;

                    // read the data
                    self.serial.read_exact(&mut buff[0..size]).await?;

                    start_count += 2;

                    //read the CRC32
                    self.serial
                        .read_exact(&mut self.buff[start_count..start_count + CRC32_LEN])
                        .await?;

                    // reading CRC32
                    let recv_crc: u32 = (self.buff[start_count + 3] as u32) << 24
                        | (self.buff[start_count + 2] as u32) << 16
                        | (self.buff[start_count + 1] as u32) << 8
                        | (self.buff[start_count] as u32);

                    let computed_crc = self.crc.compute_crc32(&buff[0..size]);

                    if recv_crc != computed_crc {
                        return Err(tokio_serial::Error::new(
                            tokio_serial::ErrorKind::InvalidInput,
                            format!(
                                "CRC does not match Received {:02X?} Computed {:02X?}",
                                recv_crc, computed_crc
                            ),
                        ));
                    }

                    return Ok(size);
                }
            } else {
                // We did not find a preamble, giving up
                return Ok(0);
            }
        }
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

        let crc32 = self.crc.compute_crc32(buff).to_ne_bytes();

        // Write the preamble
        self.serial.write_all(&PREAMBLE).await?;

        let wire_size: u16 = buff.len() as u16;

        let size_bytes = wire_size.to_ne_bytes();

        // Write the len
        self.serial.write_all(&size_bytes).await?;

        // Write the data
        self.serial.write_all(buff).await?;

        //Write the CRC32
        self.serial.write_all(&crc32).await?;

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
