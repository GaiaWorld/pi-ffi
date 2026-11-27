# 共享适配库（shared adapter library）定位

源码根为../crates；公开声明为pi_ffi_adapters/src/lib.rs的library_address，配对入口为lib_inner.rs的同名函数（function）。当前只有声明和未实现占位（unimplemented stub），本次构建结果尚待取得。

宿主（host）与插件（plugin）需要实际使用同一份共享适配库（shared adapter library）。取一个函数（function）的代码地址可能得到调用方内的导入跳板（import thunk），因此新增无参数Rust查询，返回库内私有只读静态对象（static object）的地址。加载器（loader）继续核该地址所属模块（module）、核准文件和实际映射（mapping）。

准确返回类型（type）为*const core::ffi::c_void。私有对象名LIBRARY_ADDRESS，类型（type）u8，大小1字节、值0。查询不依赖对象内容、不解引用或写入，也不分配、不加锁、不阻塞。多个线程（thread）可在同一映射（mapping）仍受保护时重复查询；不跨卸载（unloading）缓存，不将地址当作跨进程（process）稳定身份。

对普通调用方，原准备步骤保持。三类C管理接口（C API）和现有各版本输入、状态（state）及关闭语义保持。完成实现后须同次重建宿主（host）、插件（plugin）和共享适配库（shared adapter library）；不使用旧函数（function）地址作为失败回退（fallback）。

备选是在加载器（loader）解析受限导入跳板（import thunk）及导入地址表（IAT）；其平台分支和维护范围更大，当前采用库内数据地址查询。尚未实现新查询或加载接入，也未验证Windows原生行为。
