//! Regression: linking any syscall object on MSVC used to require the guest
//! allocator even though the host program never called it.

#[cfg(not(target_os = "zkvm"))]
#[test]
fn host_can_link_syscalls_without_a_guest_allocator() {
    std::hint::black_box(risc0_zkvm_platform::syscall::sys_cycle_count as extern "C" fn() -> u64);
}
