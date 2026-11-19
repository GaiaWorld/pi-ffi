use pi_ffi::{
    abi::{MetadataV1, UuidV1},
    plugin::{Declaration, Exports},
};

/// 元信息（metadata）独立于初始化，重复读取保持完整版本和插件标识。
#[test]
fn test_metadata_before_initialization_keeps_full_version() {
    static PLUGIN: Exports<()> = Exports::new(Declaration {
        uuid: [23; 16],
        version: "5.6.7-pre.2+build.13",
    });
    for _ in 0..2 {
        let mut out = std::mem::MaybeUninit::<MetadataV1>::uninit();
        // SAFETY: 输出槽（output slot）独占且长度足够。声明及版本字节具有静态存续。
        let outcome = unsafe { PLUGIN.metadata(out.as_mut_ptr()) };
        assert_eq!(outcome.kind, 0);
        assert_eq!(outcome.diagnostic_failure_kind, 0);
        assert!(outcome.diagnostic.context.is_null());
        // SAFETY: 成功返回表示已经完整写入。版本仍归同一静态声明。
        let out = unsafe { out.assume_init() };
        assert_eq!(out.uuid.bytes, [23; 16]);
        assert_eq!(out.version_len, b"5.6.7-pre.2+build.13".len() as u64);
        assert!(!out.version.is_null());
        // SAFETY: 成功的元信息（metadata）绑定有效静态字节，已核准确长度。
        let version = unsafe { std::slice::from_raw_parts(out.version, out.version_len as usize) };
        assert_eq!(version, b"5.6.7-pre.2+build.13");
    }
}

/// 校验未通过时保留原输出槽，不需要分配来源诊断（source diagnostic）。
#[test]
fn test_metadata_rejects_invalid_declarations_without_output() {
    let declarations = [
        Declaration {
            uuid: [0; 16],
            version: "1.2.3",
        },
        Declaration {
            uuid: [23; 16],
            version: "bad-version",
        },
    ];
    for declaration in declarations {
        let plugin = Exports::<()>::new(declaration);
        let original = b"original";
        let mut out = MetadataV1 {
            uuid: UuidV1 { bytes: [9; 16] },
            version: original.as_ptr(),
            version_len: original.len() as u64,
        };
        // SAFETY: 输出槽（output slot）真实可写，调用不需要任何初始化借用。
        let result = unsafe { plugin.metadata(&mut out) };
        assert_ne!(result.kind, 0);
        assert_ne!(result.diagnostic_failure_kind, 0);
        assert!(result.diagnostic.context.is_null());
        assert_eq!(out.uuid.bytes, [9; 16]);
        assert_eq!(out.version, original.as_ptr());
        assert_eq!(out.version_len, original.len() as u64);
    }
}
