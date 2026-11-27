# 共享适配库（shared adapter library）地址

源码根为../crates。当前主题只涉及pi_ffi_adapters的library_address与pi_ffi的加载核对。

- 共享适配库（shared adapter library）：宿主（host）与插件（plugin）实际共同装入的同次配套Rust动态库（dynamic library），原生输入的代码及类型（type）须匹配。
- 库内只读对象：共享适配库（shared adapter library）拥有的一字节静态对象（static object），内容不作为身份依据；其存储地址只在实际映射（mapping）受保护期间使用。
- 地址查询：取得库内对象地址以交给系统查询模块归属（module ownership）；不取得业务对象访问权限或库的所有权（ownership）。

服务（service）注册（registration）和初始化（initialization）是插件（plugin）内部的两个操作，不能由地址查询推定就绪。
