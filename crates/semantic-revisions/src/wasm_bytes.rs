//! The gateway owns these buffers: allocate, copy input, evaluate, copy output, then free both.

#[unsafe(no_mangle)]
pub extern "C" fn revision_alloc(length: u32) -> u32 {
    Box::into_raw(vec![0_u8; length as usize].into_boxed_slice()).cast::<u8>() as u32
}

/// The caller supplies a live allocation from revision_alloc and keeps it alive during evaluation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn revision_evaluate(pointer: u32, length: u32) -> u64 {
    // Both addresses originate in this instance's allocation API; no host pointer crosses WASM memory.
    let input = unsafe { std::slice::from_raw_parts(pointer as *const u8, length as usize) };
    let output = crate::evaluate_revision_policy(input).into_boxed_slice();
    let length = output.len() as u64;
    let pointer = Box::into_raw(output).cast::<u8>() as u32;
    (length << 32) | u64::from(pointer)
}

/// Free exactly one still-live allocation returned by revision_alloc or revision_evaluate.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn revision_free(pointer: u32, length: u32) {
    let slice = std::ptr::slice_from_raw_parts_mut(pointer as *mut u8, length as usize);
    unsafe {
        drop(Box::from_raw(slice));
    }
}
