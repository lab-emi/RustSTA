// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The allocator configuration of the `ruststa` program (`ruststa_cli::memory`, AGENTS rule 9.4):
//! with the mmap threshold fixed, the pages of a freed block of at least the threshold leave the
//! resident set, also after a larger block has been freed (which raises glibc's own threshold) and
//! between blocks that are still allocated. One test in its own process, so that no other test
//! allocates while it measures.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn freed_blocks_of_the_threshold_leave_the_resident_set() {
    use ruststa_cli::memory::{MMAP_THRESHOLD, configure};

    fn resident_kib() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").expect("proc status");
        status
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .expect("VmRSS")
    }

    assert!(configure(), "the C library refused the mmap threshold");
    // Without the fixed threshold, freeing this block would raise glibc's threshold to its size,
    // and the blocks below would be cut from the thread's arena instead.
    drop(std::hint::black_box(vec![1u8; 8 * MMAP_THRESHOLD]));
    let before = resident_kib();
    // Each block is followed by a small one that stays allocated, so that an arena could not
    // give the freed blocks back from its top.
    let (blocks, pins): (Vec<Vec<u8>>, Vec<Vec<u8>>) = (0..32u8)
        .map(|i| {
            let block = std::hint::black_box(vec![i + 1; 2 * MMAP_THRESHOLD]);
            (block, std::hint::black_box(vec![i; 4096]))
        })
        .unzip();
    let held = resident_kib();
    assert!(
        held >= before + 60 * 1024,
        "the 64 MiB of blocks are resident: {before} -> {held} KiB"
    );
    drop(blocks);
    let after = resident_kib();
    assert!(
        after <= before + 4 * 1024,
        "the freed blocks left the resident set: {before} -> {held} -> {after} KiB"
    );
    assert!(pins.iter().enumerate().all(|(i, p)| p[4095] as usize == i));
}
