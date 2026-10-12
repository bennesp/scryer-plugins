//! Streaming SHA-256 handles for dependencies whose host-hash feature is
//! unified by sevenz-turbo. The archive world has no SHA-256 import.

use lzma_turbo::{crypto::rustcrypto::Sha256, hooks::HostSha256Handle};
use std::{cell::RefCell, collections::HashMap};

#[derive(Default)]
struct States {
    next: u64,
    live: HashMap<u64, Sha256>,
}

thread_local! {
    static STATES: RefCell<States> = RefCell::new(States::default());
}

fn insert(hash: Sha256) -> HostSha256Handle {
    STATES.with_borrow_mut(|states| {
        let id = states.next;
        states.next = id.checked_add(1).expect("SHA-256 handle space exhausted");
        states.live.insert(id, hash);
        HostSha256Handle(id)
    })
}

pub(super) fn init() -> HostSha256Handle {
    insert(Sha256::new())
}

pub(super) fn clone(handle: HostSha256Handle) -> HostSha256Handle {
    let hash = STATES.with_borrow(|states| {
        states
            .live
            .get(&handle.0)
            .expect("live SHA-256 handle")
            .clone()
    });
    insert(hash)
}

pub(super) fn update(handle: HostSha256Handle, data: &[u8]) {
    STATES.with_borrow_mut(|states| {
        states
            .live
            .get_mut(&handle.0)
            .expect("live SHA-256 handle")
            .update(data);
    });
}

fn take(handle: HostSha256Handle) -> Sha256 {
    STATES.with_borrow_mut(|states| states.live.remove(&handle.0).expect("live SHA-256 handle"))
}

pub(super) fn finalize(handle: HostSha256Handle) -> [u8; 32] {
    take(handle).finalize()
}

pub(super) fn release(handle: HostSha256Handle) {
    drop(take(handle));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_clones_are_independent_and_all_handles_are_released() {
        let first = init();
        update(first, b"a");
        let second = clone(first);
        let abandoned = clone(first);
        release(abandoned);
        update(first, b"bc");
        update(second, b"");
        update(second, b"bc");
        let expected = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(finalize(first), expected);
        assert_eq!(finalize(second), expected);
        STATES.with_borrow(|states| assert!(states.live.is_empty()));
    }
}
