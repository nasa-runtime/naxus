use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};

use naxus_core::ArtifactDigest;

use crate::instance::InstanceLock;

pub(crate) type ActiveReaders = Arc<Mutex<BTreeMap<ArtifactDigest, usize>>>;

/// 历史修订已经被明确撤销的持久访问结果，不允许用另一修订替代。
#[derive(Debug)]
pub struct ArtifactGone;

impl std::fmt::Display for ArtifactGone {
    /// 业务作用：提供不暴露本地路径的已清除原因。
    /// 参数说明：
    /// - `formatter`：错误输出目标。
    ///
    /// 返回：固定的历史访问撤销提示。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("artifact revision was permanently cleared")
    }
}

impl std::error::Error for ArtifactGone {}

/// 文件读取期间的对象保护，同时保留独占数据卷的实例所有权。
pub struct ObjectReadLease {
    pub(crate) digest: ArtifactDigest,
    pub(crate) readers: ActiveReaders,
    pub(crate) _instance: Arc<InstanceLock>,
}

/// 已核验对象文件与持续到读取结束的生命周期保护。
pub struct ObjectReader {
    pub(crate) file: File,
    pub(crate) lease: ObjectReadLease,
}

impl ObjectReader {
    /// 业务作用：将已核验文件转交异步网络传输，同时显式转交活动读取保护。
    /// 参数说明: 无。
    ///
    /// 返回：文件与必须持有到传输结束的保护守卫。
    pub fn into_parts(self) -> (File, ObjectReadLease) {
        (self.file, self.lease)
    }

    /// 业务作用：为响应长度提供已经打开的对象元数据，避免重新按路径读取另一代文件。
    /// 参数说明: 无。
    ///
    /// 返回：当前文件句柄的真实元数据。
    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self.file.metadata()
    }
}

impl Read for ObjectReader {
    /// 业务作用：在活动读取保护下提供原始制品字节。
    /// 参数说明：
    /// - `buffer`：调用方接收区。
    ///
    /// 返回：实际读取字节数，底层 I/O 失败向上传播。
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read(buffer)
    }
}

impl Seek for ObjectReader {
    /// 业务作用：允许归档检查在同一受保护文件中定位，不重新打开摘要路径。
    /// 参数说明：
    /// - `position`：同一文件中的目标位置。
    ///
    /// 返回：新文件偏移，非法定位返回底层错误。
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.file.seek(position)
    }
}

impl Drop for ObjectReadLease {
    /// 业务作用：读取结束后释放对象活动计数，使后续清除能够重新评估安全窗口。
    /// 参数说明: 无。
    ///
    /// 返回：只减少本守卫所属摘要的计数；门禁不可用时保守保留保护。
    fn drop(&mut self) {
        if let Ok(mut readers) = self.readers.lock() {
            if let Some(count) = readers.get_mut(&self.digest) {
                if *count <= 1 {
                    readers.remove(&self.digest);
                } else {
                    *count -= 1;
                }
            }
        }
    }
}
