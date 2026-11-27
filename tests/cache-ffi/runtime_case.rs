//! 宿主（host）拥有并推进实际异步运行时（runtime），检查插件取得的独立持有。
use futures::FutureExt;
use pi_async_runtime::{CloseState, ConcurrentRuntime, Handle, RuntimeControl, RuntimeQuery, RuntimeStatus, TaskDispatch};
use std::{fs, path::Path, pin::pin, sync::{Arc, mpsc}, thread};

pub struct HostRuntime {
    handle: Arc<Handle>,
    owner: thread::JoinHandle<()>,
    drive: mpsc::Sender<()>,
    driven: mpsc::Receiver<()>,
}

impl HostRuntime {
    /// 在独立所属线程（owner thread）上创建真实运行时（runtime），限时等待其句柄（handle）。
    pub fn new() -> Self {
        let (publish, handle) = mpsc::sync_channel(1);
        let (drive, requests) = mpsc::channel();
        let (finished, driven) = mpsc::sync_channel(1);
        // 每个原运行时由各自的所属线程（owner thread）推进，已替换的实例保持开放。
        let owner = thread::spawn(move || {
            let mut runtime = ConcurrentRuntime::new().unwrap();
            publish.send(Arc::new(runtime.handle())).unwrap();
            while requests.recv().is_ok() {
                runtime.run_once().unwrap();
                finished.send(()).unwrap();
            }
            runtime.request_close();
            runtime.run().unwrap();
            assert_eq!(runtime.status(), RuntimeStatus::Closed);
            assert_eq!(runtime.close_state(), CloseState::Completed);
        });
        let handle = handle.recv_timeout(super::STAGE_TIMEOUT).unwrap();
        Self { handle, owner, drive, driven }
    }

    /// 为标准初始化入口提供同一实例的共享持有，宿主（host）保留原句柄（handle）。
    pub fn input(&self) -> Arc<Handle> {
        self.handle.clone()
    }

    /// 只有宿主推进之后，插件实际提交的任务（task）才可产生本轮记录。
    pub fn expect_plugin_task(&self, root: &Path, label: &str) {
        let marker = root.join(format!("runtime-task-{label}"));
        assert!(!marker.exists());
        assert_eq!(self.handle.external_task_count(), 1);
        let actual = super::wait_for("插件异步任务（task）完成", || {
            self.run_once();
            match fs::read_to_string(&marker) {
                Ok(value) => Some(value),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("{error}"),
            }
        });
        let id = self.handle.id().get();
        assert_eq!(actual, format!("{id} {:?}\n", self.owner.thread().id()));
        assert_eq!(self.handle.external_task_count(), 0);
        println!("host runtime_task label={label} id={id} owner_verified=true");
    }

    /// 关闭插件后，宿主（host）仍能通过原句柄（handle）完成自己的任务。
    pub fn expect_returned_input(&self, label: &str) {
        assert_eq!(Arc::strong_count(&self.handle), 1);
        assert_eq!(self.handle.status(), RuntimeStatus::Open);
        let mut task = pin!(self.handle.spawn(async { 73 }).unwrap_or_else(|_| panic!("宿主任务提交失败")));
        let actual = super::wait_for("宿主任务（task）完成", || {
            self.run_once();
            task.as_mut().now_or_never()
        });
        assert_eq!(actual.unwrap(), 73);
        println!("host runtime_after_close label={label} id={} input_returned=true owner_task=true", self.handle.id().get());
    }

    /// 由宿主（host）通知所属线程（owner thread）完成原运行时的关闭并等待该线程退出。
    pub fn close(self) {
        assert_eq!(self.handle.status(), RuntimeStatus::Open);
        drop(self.drive);
        self.owner.join().unwrap();
    }

    /// 请所属线程（owner thread）推进一次运行时（runtime），并在阶段时限内确认返回。
    fn run_once(&self) {
        self.drive.send(()).unwrap();
        self.driven.recv_timeout(super::STAGE_TIMEOUT).unwrap();
    }
}
