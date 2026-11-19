use pi_ffi_core::{boundary, diagnostic};
use pi_result::{Error, ErrorKind};

/// 通过公开诊断（diagnostic）入口核对协议规定的分类编号及无来源失败。
#[test]
fn test_status_projection_and_limited_failure() {
    use ErrorKind::*;

    let expected = [
        InvalidInput,
        InvalidState,
        NotFound,
        AlreadyExists,
        Conflict,
        PermissionDenied,
        Io,
        Timeout,
        Cancelled,
        Busy,
        Unavailable,
        Unsupported,
        ResourceExhausted,
        Corrupted,
        Dependency,
        Internal,
        Other,
    ];
    for (index, kind) in expected.into_iter().enumerate() {
        assert_eq!(diagnostic::status_code(kind), index as u32 + 1);
    }

    // SAFETY: 两种结果均没有独立诊断持有（diagnostic holding）或来源地址。
    unsafe {
        diagnostic::decode_management(None, diagnostic::success_outcome()).unwrap();
        let failure = diagnostic::limited_outcome(Conflict, ResourceExhausted);
        assert_eq!(failure.kind, 5);
        assert_eq!(failure.diagnostic_failure_kind, 13);
        let error = diagnostic::decode_management(None, failure).unwrap_err();
        assert_eq!(error.current_context(), &Conflict);
        assert!(diagnostic::copied_diagnostic(&error).is_none());
    }
}

/// 本地报告（report）保留三段原文及原编号，可在另一线程（thread）读取。
#[test]
fn test_copied_diagnostic_keeps_all_source_text() {
    let error = diagnostic::CopiedDiagnostic {
        kind: ErrorKind::Other,
        user: "原始用户文本".to_owned(),
        verbose: "\n原始完整文本\0".to_owned(),
        debug: "原始调试文本\n".to_owned(),
        raw_kind: 90,
    }
    .into_error();

    std::thread::spawn(move || {
        assert_eq!(error.current_context(), &ErrorKind::Other);
        let copied = diagnostic::copied_diagnostic(&error).unwrap();
        assert_eq!(copied.kind, ErrorKind::Other);
        assert_eq!(copied.raw_kind, 90);
        assert_eq!(copied.user, "原始用户文本");
        assert_eq!(copied.verbose, "\n原始完整文本\0");
        assert_eq!(copied.debug, "原始调试文本\n");
    })
    .join()
    .unwrap();
}

/// 经本地保护（local boundary protection）转换可展开恐慌（panic），并保留普通错误。
#[test]
fn test_unwinding_and_ordinary_error() {
    let context = || boundary::Context {
        operation: "test operation",
        module_epoch: 7,
        round: 3,
        source: 11,
    };
    let error = boundary::catch::<()>(context(), || panic!("检查本地恐慌")).unwrap_err();
    assert_eq!(error.current_context(), &ErrorKind::Internal);
    assert!(pi_result::debug_message(&error).contains("检查本地恐慌"));

    let error = boundary::catch::<()>(context(), || Err(Error::new(ErrorKind::Busy).attach(29u32)))
        .unwrap_err();
    assert_eq!(error.current_context(), &ErrorKind::Busy);
    assert_eq!(error.downcast_ref::<u32>(), Some(&29));

    let failure = diagnostic::protect_outcome(context(), || panic!("C 边界保护"));
    assert_eq!(failure.kind, 16);
    assert_eq!(failure.diagnostic_failure_kind, 16);
    assert!(failure.diagnostic.context.is_null());
    assert!(failure.diagnostic.api.is_null());
    assert_eq!(
        diagnostic::protect_status(context(), || panic!("有限状态")),
        16
    );
}
