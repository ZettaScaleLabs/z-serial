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

use rand::Rng;
use z_serial::CRC32;

const MAX_ITERATIONS: usize = 4096;

// FIXME: fix this test
// #[test]
// fn check_test_vectors() {
//     // (:digest-test #a"" #h"00000000")
//     // (:digest-test #a"a" #h"e8b7be43")
//     // (:digest-test #a"abc" #h"352441c2")
//     // (:digest-test #a"message digest" #h"20159d7f")
//     // (:digest-test #a"abcdefghijklmnopqrstuvwxyz" #h"4c2750bd")
//     // (:digest-test #a"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789" #h"1fc2e6d2")
//     // (:digest-test #a"12345678901234567890123456789012345678901234567890123456789012345678901234567890" #h"7ca94a72")

//     let data: Vec<u8> = vec![];
//     let crc = CRC32::default();

//     let empty_crc = 0u32;
//     assert_eq!(crc.compute_crc32(&data), empty_crc);

//     let data: Vec<u8> = String::from("a").into_bytes();
//     let a_crc = 0xe8b7be43;
//     assert_eq!(crc.compute_crc32(&data), a_crc);
// }

#[test]
fn check_same_crc() {
    let mut rng = rand::thread_rng();
    let crc = CRC32::default();

    for _ in 0..MAX_ITERATIONS {
        let data_size = rng.gen_range(100..1000);

        let data: Vec<u8> = (0..data_size)
            .map(|_| rng.gen_range(0u8..u8::MAX))
            .collect();

        assert_eq!(crc.compute_crc32(&data), crc.compute_crc32(&(data.clone())))
    }
}

#[test]
fn check_different_crc() {
    let mut rng = rand::thread_rng();
    let crc = CRC32::default();

    for _ in 0..MAX_ITERATIONS {
        let data_one_size = rng.gen_range(100..1000);

        let data_two_size = rng.gen_range(100..1000);

        let data_one: Vec<u8> = (0..data_one_size)
            .map(|_| rng.gen_range(0u8..u8::MAX))
            .collect();

        let data_two: Vec<u8> = (0..data_two_size)
            .map(|_| rng.gen_range(0u8..u8::MAX))
            .collect();

        assert_ne!(crc.compute_crc32(&data_one), crc.compute_crc32(&data_two))
    }
}

#[test]
fn check_different_bitflips_crc() {
    let mut rng = rand::thread_rng();
    let crc = CRC32::default();

    for _ in 0..MAX_ITERATIONS {
        let data_size = rng.gen_range(100..1000);

        let data: Vec<u8> = (0..data_size)
            .map(|_| rng.gen_range(0u8..u8::MAX))
            .collect();

        let mut bitflipped = data.clone();

        // Random flipping 5 bytes
        for _ in 0..5 {
            let index = rng.gen_range(0..data_size);

            bitflipped[index] = !bitflipped[index];
        }

        assert_ne!(data, bitflipped);

        assert_ne!(crc.compute_crc32(&data), crc.compute_crc32(&bitflipped))
    }
}
