// 本库拥有的一字节只读静态对象（static object）；内容不作为身份依据。
// 非零大小保证存在实际存储，存续期随本库的实际映射（mapping），不携带业务状态（business state）。
static LIBRARY_ADDRESS: u8 = 0;

pub(super) fn library_address() -> *const core::ffi::c_void {
    core::ptr::addr_of!(LIBRARY_ADDRESS).cast()
}
