use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use nasa::tx::sqlite::{
    self, DataSourceConfig, SqliteDataSource, SqliteDataSourceOwner, TxDecision,
};
use sqlx::{Column, Row as SqlxRow, TypeInfo, ValueRef};
use tokio::sync::mpsc as channel;

use crate::{
    instance::InstanceLock,
    statements::{Mappers, Output, Statement},
    StoreOptions,
};

/// 仓库 SQL 参数的拥有型表示，整数越界不会截断为另一个持久值。
#[derive(Clone)]
pub(crate) enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    Overflow,
}

pub(crate) trait ToValue {
    /// 业务作用：在同步调用与异步 Mapper 之间传递独立参数，避免借用调用栈中的临时数据。
    /// 参数说明: 无。
    /// 返回：保留 NULL、字节和整数范围的拥有型值。
    fn to_value(&self) -> Value;
}

macro_rules! integer_values {
    ($($kind:ty),* $(,)?) => {$ (
        impl ToValue for $kind {
            /// 业务作用：保留数据库整数范围，拒绝无法持久表示的业务数值。
            /// 参数说明: 无。
            /// 返回：有符号整数或显式越界标识。
            fn to_value(&self) -> Value {
                i64::try_from(*self).map(Value::Integer).unwrap_or(Value::Overflow)
            }
        }
    )*};
}
integer_values!(i32, i64, u32, u64, usize);

impl ToValue for bool {
    /// 业务作用：按 SQLite 布尔字段约定保存业务开关。
    /// 参数说明: 无。
    /// 返回：零或一。
    fn to_value(&self) -> Value {
        Value::Integer(i64::from(*self))
    }
}
impl ToValue for str {
    /// 业务作用：复制业务文本以越过异步执行边界。
    /// 参数说明: 无。
    /// 返回：与输入内容一致的拥有型文本。
    fn to_value(&self) -> Value {
        Value::Text(self.to_owned())
    }
}
impl ToValue for String {
    /// 业务作用：保持业务字符串身份并独立持有 SQL 参数。
    /// 参数说明: 无。
    /// 返回：文本副本。
    fn to_value(&self) -> Value {
        self.as_str().to_value()
    }
}
impl ToValue for [u8] {
    /// 业务作用：完整保留代理索引的二进制内容。
    /// 参数说明: 无。
    /// 返回：独立 BLOB 参数，空字节串不变成 NULL。
    fn to_value(&self) -> Value {
        Value::Blob(self.to_vec())
    }
}
impl ToValue for Vec<u8> {
    /// 业务作用：复制业务缓存字节，保证异步写入不依赖调用者缓冲区。
    /// 参数说明: 无。
    /// 返回：保留完整内容的 BLOB。
    fn to_value(&self) -> Value {
        self.as_slice().to_value()
    }
}
impl<T: ToValue + ?Sized> ToValue for &T {
    /// 业务作用：从只读业务借用取得不依赖调用栈的 SQL 参数。
    /// 参数说明: 无。
    /// 返回：底层业务值的拥有型表示。
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}
impl<T: ToValue> ToValue for Option<T> {
    /// 业务作用：区分缺失字段与空字符串、零数值。
    /// 参数说明: 无。
    /// 返回：缺失时为 NULL，存在时保留内部值。
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, ToValue::to_value)
    }
}

macro_rules! params {
    ($($value:expr),* $(,)?) => {
        vec![$($crate::database::ToValue::to_value(&$value)),*]
    };
}
pub(crate) use params;

pub(crate) trait Parameters {
    /// 业务作用：在进入 Mapper 执行域之前固定全部绑定值。
    /// 参数说明: 无。
    /// 返回：按 SQL 参数次序排列的拥有型绑定。
    fn values(self) -> Vec<Value>;
}
impl Parameters for Vec<Value> {
    /// 业务作用：转交已经固定的业务绑定。
    /// 参数说明: 无。
    /// 返回：原绑定集合，不改变次序。
    fn values(self) -> Vec<Value> {
        self
    }
}
impl<T: ToValue, const N: usize> Parameters for [T; N] {
    /// 业务作用：统一定长业务参数与 Mapper 绑定的所有权边界。
    /// 参数说明: 无。
    /// 返回：按输入次序复制的参数。
    fn values(self) -> Vec<Value> {
        self.iter().map(ToValue::to_value).collect()
    }
}

impl sqlx::Type<sqlx::Sqlite> for Value {
    /// 业务作用：为拥有型绑定提供 SQLx 类型入口，实际存储类型由各个值的编码决定。
    /// 参数说明: 无。
    /// 返回：文本类型占位；不参与结果解码。
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
}
impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for Value {
    /// 业务作用：保持参数的 SQLite 原始类型，避免把数值或 NULL 转成文本拼接进 SQL。
    /// 参数说明：
    /// - `buffer`：SQLx 当前语句的参数缓冲。
    ///
    /// 返回：成功绑定值；超出 SQLite 整数范围时拒绝执行。
    fn encode_by_ref(
        &self,
        buffer: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer,
    ) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        match self {
            Self::Null => <Option<String> as sqlx::Encode<'q, sqlx::Sqlite>>::encode(None, buffer),
            Self::Integer(value) => <i64 as sqlx::Encode<'q, sqlx::Sqlite>>::encode(*value, buffer),
            Self::Real(value) => <f64 as sqlx::Encode<'q, sqlx::Sqlite>>::encode(*value, buffer),
            Self::Text(value) => {
                <String as sqlx::Encode<'q, sqlx::Sqlite>>::encode(value.clone(), buffer)
            }
            Self::Blob(value) => {
                <Vec<u8> as sqlx::Encode<'q, sqlx::Sqlite>>::encode(value.clone(), buffer)
            }
            Self::Overflow => Err("repository integer exceeds SQLite range".into()),
        }
    }
}

/// Mapper 交付的拥有型行，数据库句柄在业务解析前已经归还。
pub(crate) struct Row(Vec<Value>);
impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for Row {
    /// 业务作用：复制有限结果集中的列值，使同步业务解析不持有数据库连接或事务借用。
    /// 参数说明：
    /// - `row`：Mapper 已完成执行的 SQLite 行。
    ///
    /// 返回：保留列类型的独立行；不支持的存储类型拒绝交付。
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> sqlx::Result<Self> {
        let mut values = Vec::with_capacity(row.len());
        for index in 0..row.len() {
            let raw = row.try_get_raw(index)?;
            values.push(if raw.is_null() {
                Value::Null
            } else {
                match raw.type_info().name() {
                    "INTEGER" | "INT" | "BOOLEAN" => Value::Integer(row.try_get(index)?),
                    "REAL" => Value::Real(row.try_get(index)?),
                    "TEXT" => Value::Text(row.try_get(index)?),
                    "BLOB" => Value::Blob(row.try_get(index)?),
                    _ => {
                        return Err(sqlx::Error::Decode(
                            format!("unsupported repository column {}", row.column(index).name())
                                .into(),
                        ))
                    }
                }
            });
        }
        Ok(Self(values))
    }
}
pub(crate) trait FromValue: Sized {
    /// 业务作用：将存储列转换为业务字段，禁止 NULL 或数值溢出静默改变业务身份。
    /// 参数说明：
    /// - `value`：当前列的原始值。
    ///
    /// 返回：类型与取值范围匹配的业务值，否则返回解码失败。
    fn from_value(value: &Value) -> sqlx::Result<Self>;
}
macro_rules! decode_integers {
    ($($kind:ty),* $(,)?) => {$ (
        impl FromValue for $kind {
            /// 业务作用：按业务字段范围读取持久整数。
            /// 参数说明：
            /// - `value`：原始 SQLite 列。
            ///
            /// 返回：无截断整数；类型或范围不匹配时失败。
            fn from_value(value: &Value) -> sqlx::Result<Self> {
                if let Value::Integer(value) = value {
                    return Self::try_from(*value).map_err(|error| sqlx::Error::Decode(error.into()));
                }
                Err(sqlx::Error::Decode("repository column is not an integer".into()))
            }
        }
    )*};
}
decode_integers!(i32, i64, u32, u64, usize);
impl FromValue for String {
    /// 业务作用：严格读取业务文本，避免隐式类型转换掩盖数据损坏。
    /// 参数说明：
    /// - `value`：原始列值。
    ///
    /// 返回：文本副本；非文本拒绝。
    fn from_value(value: &Value) -> sqlx::Result<Self> {
        match value {
            Value::Text(value) => Ok(value.clone()),
            _ => Err(sqlx::Error::Decode("repository column is not text".into())),
        }
    }
}
impl FromValue for Vec<u8> {
    /// 业务作用：读取代理缓存的原始字节，不做文本编码转换。
    /// 参数说明：
    /// - `value`：Mapper 交付的原始列。
    ///
    /// 返回：BLOB 内容；其它类型拒绝。
    fn from_value(value: &Value) -> sqlx::Result<Self> {
        match value {
            Value::Blob(value) => Ok(value.clone()),
            _ => Err(sqlx::Error::Decode(
                "repository column is not a blob".into(),
            )),
        }
    }
}
impl FromValue for bool {
    /// 业务作用：将 SQLite 整数状态读取为业务布尔值。
    /// 参数说明：
    /// - `value`：原始列值。
    ///
    /// 返回：零为 false，非零为 true；非整数拒绝。
    fn from_value(value: &Value) -> sqlx::Result<Self> {
        Ok(i64::from_value(value)? != 0)
    }
}
impl<T: FromValue> FromValue for Option<T> {
    /// 业务作用：保留可选业务字段的缺失语义。
    /// 参数说明：
    /// - `value`：原始列值。
    ///
    /// 返回：NULL 为 None，其余按业务类型严格解码。
    fn from_value(value: &Value) -> sqlx::Result<Self> {
        match value {
            Value::Null => Ok(None),
            _ => T::from_value(value).map(Some),
        }
    }
}
impl Row {
    /// 业务作用：从固定 Mapper 投影读取业务字段并保留原有列顺序合同。
    /// 参数说明：
    /// - `index`：接口 SQL 中声明的列位置。
    ///
    /// 返回：解码后的值；越界、类型错误或整数溢出均失败。
    pub(crate) fn get<I: TryInto<usize>, T: FromValue>(&self, index: I) -> sqlx::Result<T> {
        let index = index
            .try_into()
            .map_err(|_| sqlx::Error::Decode("invalid column index".into()))?;
        let value = self
            .0
            .get(index)
            .ok_or(sqlx::Error::ColumnIndexOutOfBounds {
                index,
                len: self.0.len(),
            })?;
        T::from_value(value)
    }
}

pub(crate) trait OptionalExtension<T> {
    /// 业务作用：只把确认为不存在的记录映射成业务缺失，数据库失败继续向上返回。
    /// 参数说明: 无。
    /// 返回：存在值、None 或原始执行错误。
    fn optional(self) -> Result<Option<T>>;
}
impl<T> OptionalExtension<T> for Result<T> {
    /// 业务作用：区分空查询与存储不可用，避免故障被当作不存在而重新创建状态。
    /// 参数说明: 无。
    /// 返回：仅 RowNotFound 转为 None，其余错误保持错误链。
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(error)
                if matches!(
                    error.downcast_ref::<sqlx::Error>(),
                    Some(sqlx::Error::RowNotFound)
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

type Reply = mpsc::SyncSender<Result<Output>>;
enum Command {
    Statement(Statement, Vec<Value>, Reply),
    Begin(Reply),
    Finish(bool, Reply),
    Inspect(mpsc::SyncSender<Result<u32>>),
    Backup(PathBuf, Reply),
    Checkpoint(Reply),
    Close(Reply),
}

/// 每个数据卷只接纳一个同步业务持有者，异步执行始终留在宿主已有运行时。
pub(crate) struct Connection {
    sender: channel::UnboundedSender<Command>,
}
pub(crate) enum TransactionBehavior {
    Immediate,
}
pub(crate) struct Transaction<'a> {
    connection: &'a Connection,
    finished: bool,
}
pub(crate) struct Prepared<'a> {
    connection: &'a Connection,
    statement: Statement,
}

impl Connection {
    /// 业务作用：在持有实例权威后打开 NASA 文件来源，将异步 SQL 责任交给宿主运行时。
    /// 参数说明：
    /// - `root`：已规范化的私有仓库根。
    /// - `options`：业务容量与等待预算。
    /// - `instance`：覆盖数据库与对象生命周期的独占权。
    ///
    /// 返回：固定来源的串行执行域；没有多线程宿主运行时或文件不安全时拒绝。
    pub(crate) fn open(
        root: &Path,
        options: &StoreOptions,
        instance: Arc<InstanceLock>,
    ) -> Result<Self> {
        let runtime = tokio::runtime::Handle::try_current()
            .context("Store requires a Tokio multi-thread runtime and a blocking caller")?;
        ensure!(
            runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread,
            "Store requires a multi-thread runtime"
        );
        protect_database(root)?;
        let source_config = datasource_config(root, options);
        let owner = runtime.block_on(sqlite::open_owned("naxus", source_config))?;
        Self::start(owner.datasource(), Some(owner), instance, &runtime)
    }

    /// 业务作用：复用 Application 已创建的来源，由框架统一负责数据库排空与关闭。
    /// 参数说明：
    /// - `source`：配置名为 naxus 的受管 SQLite 来源。
    /// - `instance`：元数据和对象目录共同持有的实例权威。
    ///
    /// 返回：共享宿主运行时的 Mapper 执行域，不额外创建数据源。
    pub(crate) fn managed(source: SqliteDataSource, instance: Arc<InstanceLock>) -> Result<Self> {
        let runtime = tokio::runtime::Handle::try_current()?;
        Self::start(source, None, instance, &runtime)
    }

    /// 业务作用：为所有 Mapper 固定来源并启动单一串行命令消费任务。
    /// 参数说明：
    /// - `source`：固定 owner 的业务来源。
    /// - `owner`：独立宿主时持有的关闭责任。
    /// - `instance`：执行任务退出前持续持有的数据卷独占权。
    /// - `runtime`：现有宿主运行时。
    ///
    /// 返回：绑定后的命令入口，不建立额外运行时或连接池。
    fn start(
        source: SqliteDataSource,
        owner: Option<SqliteDataSourceOwner>,
        instance: Arc<InstanceLock>,
        runtime: &tokio::runtime::Handle,
    ) -> Result<Self> {
        // 数据库初始化失败和运行时取消都不能让旧实例协议先取得对象目录，守卫由真实文件 owner 保活。
        source.retain_file_guard(instance.clone())?;
        let mappers = Mappers::new(&source)?;
        let (sender, receiver) = channel::unbounded_channel();
        runtime.spawn(serve(source, owner, instance, mappers, receiver));
        Ok(Self {
            sender,
        })
    }

    /// 业务作用：等待本次固定 SQL 操作的最终交付，调用者持续持有同步业务门。
    /// 参数说明：
    /// - `command`：只能构造已登记 Mapper 或固定维护操作的命令。
    ///
    /// 返回：执行结果；消费任务退出时明确失败。
    fn request(&self, command: impl FnOnce(Reply) -> Command) -> Result<Output> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .send(command(reply))
            .map_err(|_| anyhow::anyhow!("repository database executor stopped"))?;
        result
            .recv()
            .context("repository database result unavailable")?
    }

    /// 业务作用：通过声明式 Mapper 写入状态，向业务返回真实影响行数。
    /// 参数说明：
    /// - `statement`：封闭集合中的写接口。
    /// - `parameters`：接口的业务绑定。
    ///
    /// 返回：确认完成的影响行数；SQL 或来源失败时保留错误链。
    pub(crate) fn execute(
        &self,
        statement: Statement,
        parameters: impl Parameters,
    ) -> Result<usize> {
        match self.request(|reply| Command::Statement(statement, parameters.values(), reply))? {
            Output::Affected(count) => Ok(usize::try_from(count)?),
            _ => anyhow::bail!("repository write interface returned rows"),
        }
    }

    /// 业务作用：读取可判定存在性的单条业务记录。
    /// 参数说明：
    /// - `statement`：固定查询接口。
    /// - `parameters`：查询条件。
    /// - `decode`：业务字段解析。
    ///
    /// 返回：首行的业务值；无记录时保留 RowNotFound，执行或解码失败时返回错误。
    pub(crate) fn query_row<T>(
        &self,
        statement: Statement,
        parameters: impl Parameters,
        decode: impl FnOnce(&Row) -> sqlx::Result<T>,
    ) -> Result<T> {
        match self.request(|reply| Command::Statement(statement, parameters.values(), reply))? {
            Output::Rows(rows) => Ok(decode(rows.first().ok_or(sqlx::Error::RowNotFound)?)?),
            _ => anyhow::bail!("repository read interface returned a write result"),
        }
    }

    /// 业务作用：绑定固定结果集接口，不向业务暴露 SQL 文本或原生 statement。
    /// 参数说明：
    /// - `statement`：已登记的 Mapper 查询。
    ///
    /// 返回：只持有接口身份的读取入口。
    pub(crate) fn prepare(&self, statement: Statement) -> Result<Prepared<'_>> {
        Ok(Prepared {
            connection: self,
            statement,
        })
    }

    /// 业务作用：在 SQL 消费任务内部取得 IMMEDIATE 事务，后续命令不会跨任务丢失事务上下文。
    /// 参数说明：
    /// - `_behavior`：仓库写入使用的固定锁策略。
    ///
    /// 返回：真实事务已进入后的保护对象；未显式提交的路径自动请求回滚。
    pub(crate) fn transaction_with_behavior(
        &mut self,
        _behavior: TransactionBehavior,
    ) -> Result<Transaction<'_>> {
        self.request(Command::Begin)?;
        Ok(Transaction {
            connection: self,
            finished: false,
        })
    }

    /// 业务作用：使用组件固定维护入口验证结构及外键，读取旧库格式标识。
    /// 参数说明: 无。
    /// 返回：完整性通过后的 user_version；失败阻断存储开放。
    pub(crate) fn inspect(&self) -> Result<u32> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Inspect(reply))
            .map_err(|_| anyhow::anyhow!("repository database executor stopped"))?;
        result.recv().context("repository inspection unavailable")?
    }

    /// 业务作用：在元数据和对象生命周期门内生成经过校验的独立数据库副本。
    /// 参数说明：
    /// - `path`：本次备份暂存根中的全新文件。
    ///
    /// 返回：原生复制、校验和持久发布均已确认。
    pub(crate) fn backup_to(&self, path: &Path) -> Result<()> {
        self.request(|reply| Command::Backup(path.to_owned(), reply))?;
        Ok(())
    }

    /// 业务作用：回收可安全处理的 WAL 页，不删除尚有读者依赖的日志。
    /// 参数说明: 无。
    /// 返回：PASSIVE checkpoint 已执行，忙读者可以保留未处理页。
    pub(crate) fn checkpoint(&self) -> Result<()> {
        self.request(Command::Checkpoint)?;
        Ok(())
    }
}
impl Drop for Connection {
    /// 业务作用：在数据卷实例锁释放前等待独立来源关闭，避免后台 SQL 越过实例交接。
    /// 参数说明: 无。
    /// 返回：命令消费任务已收口；失败的原生关闭仍由持有实例锁的任务继续等待。
    fn drop(&mut self) {
        let _ = self.request(Command::Close);
    }
}
impl<'a> std::ops::Deref for Transaction<'a> {
    type Target = Connection;
    /// 业务作用：让事务中的业务读取和写入复用同一个串行执行域。
    /// 参数说明: 无。
    /// 返回：已进入事务的固定来源入口。
    fn deref(&self) -> &Self::Target {
        self.connection
    }
}
impl Transaction<'_> {
    /// 业务作用：等待 NASA 确认物理提交后才向调用者报告业务成功。
    /// 参数说明: 无。
    /// 返回：确认提交才成功；拒绝或结果不确定时返回明确事务错误。
    pub(crate) fn commit(mut self) -> Result<()> {
        let result = self
            .connection
            .request(|reply| Command::Finish(true, reply));
        self.finished = true;
        result?;
        Ok(())
    }
}
impl Drop for Transaction<'_> {
    /// 业务作用：对业务提前返回、失败或展开路径撤销尚未提交的全部状态。
    /// 参数说明: 无。
    /// 返回：等待回滚收口，不把失败路径解释为提交。
    fn drop(&mut self) {
        if !self.finished {
            let _ = self
                .connection
                .request(|reply| Command::Finish(false, reply));
        }
    }
}
impl Prepared<'_> {
    /// 业务作用：消费 Mapper 有界结果后交给同步业务解析，避免文件处理期间占用行流。
    /// 参数说明：
    /// - `parameters`：业务筛选条件。
    /// - `decode`：每行的领域类型转换。
    ///
    /// 返回：独立结果迭代器；执行失败或列类型错误保持错误信息。
    pub(crate) fn query_map<T>(
        &mut self,
        parameters: impl Parameters,
        mut decode: impl FnMut(&Row) -> sqlx::Result<T>,
    ) -> Result<std::vec::IntoIter<sqlx::Result<T>>> {
        match self
            .connection
            .request(|reply| Command::Statement(self.statement, parameters.values(), reply))?
        {
            Output::Rows(rows) => Ok(rows.iter().map(&mut decode).collect::<Vec<_>>().into_iter()),
            _ => anyhow::bail!("repository collection interface returned a write result"),
        }
    }
}

/// 业务作用：把事务上下文留在唯一消费任务中，避免同步业务跨异步任务分裂原子提交。
/// 参数说明：
/// - `source`：所有 Mapper 固定的来源。
/// - `owner`：独立打开时持有的关闭责任。
/// - `_instance`：真实数据库关闭前保留的数据卷独占权。
/// - `mappers`：声明式 SQL 接口集合。
/// - `receiver`：受外层同步互斥约束的命令入口。
///
/// 返回：来源关闭后退出；事务超期后拒绝剩余命令，直到业务结束原事务。
async fn serve(
    source: SqliteDataSource,
    owner: Option<SqliteDataSourceOwner>,
    _instance: Arc<InstanceLock>,
    mappers: Mappers,
    mut receiver: channel::UnboundedReceiver<Command>,
) {
    let mut failed_transaction = false;
    let mut close_reply = None;
    while let Some(command) = receiver.recv().await {
        match command {
            Command::Begin(reply) if !failed_transaction => {
                let mut entered = false;
                let mut finish_reply = None;
                let result = sqlite::run_decided_from(
                    &source,
                    Some(sqlite::TransactionMode::Immediate),
                    async {
                        entered = true;
                        let _ = reply.send(Ok(Output::Affected(0)));
                        while let Some(command) = receiver.recv().await {
                            match command {
                                Command::Statement(statement, parameters, reply) => {
                                    let _ = reply.send(
                                        statement.run(&mappers, parameters).await.with_context(
                                            || format!("repository Mapper {statement:?}"),
                                        ),
                                    );
                                }
                                Command::Finish(commit, reply) => {
                                    finish_reply = Some(reply);
                                    return if commit {
                                        TxDecision::Commit(())
                                    } else {
                                        TxDecision::Rollback(())
                                    };
                                }
                                other => reject(
                                    other,
                                    "repository maintenance is forbidden in a transaction",
                                ),
                            }
                        }
                        TxDecision::Rollback(())
                    },
                )
                .await;
                if !entered {
                    let _ = reply.send(Err(anyhow::anyhow!(
                        "repository transaction entry failed: {result:?}"
                    )));
                } else if let Some(reply) = finish_reply {
                    let outcome = match result {
                        Ok(()) | Err(sqlite::TxRunError::Rollback(())) => Ok(Output::Affected(0)),
                        Err(error) => Err(anyhow::anyhow!(
                            "repository transaction not committed: {error:?}"
                        )),
                    };
                    let _ = reply.send(outcome);
                } else {
                    // 总期限到达后不能把原事务剩余命令当作独立写入，否则会产生半完成业务状态。
                    failed_transaction = true;
                }
            }
            Command::Finish(_, reply) => {
                failed_transaction = false;
                let _ = reply.send(Err(anyhow::anyhow!(
                    "repository transaction ended before decision"
                )));
            }
            Command::Close(reply) => {
                close_reply = Some(reply);
                break;
            }
            other if failed_transaction => {
                reject(other, "repository transaction is no longer active")
            }
            Command::Statement(statement, parameters, reply) => {
                let _ = reply.send(
                    statement
                        .run(&mappers, parameters)
                        .await
                        .with_context(|| format!("repository Mapper {statement:?}")),
                );
            }
            Command::Inspect(reply) => {
                let result = source
                    .maintenance()
                    .inspect(&budget())
                    .await
                    .map(|report| report.user_version);
                let _ = reply.send(result);
            }
            Command::Checkpoint(reply) => {
                let result = source
                    .maintenance()
                    .checkpoint(sqlite::CheckpointMode::Passive, &budget())
                    .await
                    .map(|_| Output::Affected(0));
                let _ = reply.send(result);
            }
            Command::Backup(path, reply) => {
                let result = source
                    .maintenance()
                    .backup_to(path, &budget())
                    .await
                    .map(|_| Output::Affected(0));
                let _ = reply.send(result);
            }
            Command::Begin(reply) => {
                let _ = reply.send(Err(anyhow::anyhow!("repository transaction unavailable")));
            }
        }
    }
    if let Some(owner) = owner {
        // 关闭超期不能释放实例权威；继续等待真实原生退出，后来的实例必须保持被拒绝。
        loop {
            match owner
                .close(tokio::time::Instant::now() + Duration::from_secs(60))
                .await
            {
                Ok(()) => break,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }
    if let Some(reply) = close_reply {
        let _ = reply.send(Ok(Output::Affected(0)));
    }
}

/// 业务作用：拒绝已经失去事务或维护资格的命令，让同步调用者明确结束等待。
/// 参数说明：
/// - `command`：不能执行的命令。
/// - `reason`：不含业务参数的拒绝原因。
///
/// 返回：向该命令的等待者传递失败，不执行 SQL。
fn reject(command: Command, reason: &str) {
    match command {
        Command::Inspect(reply) => {
            let _ = reply.send(Err(anyhow::anyhow!(reason.to_owned())));
        }
        Command::Statement(_, _, reply)
        | Command::Begin(reply)
        | Command::Finish(_, reply)
        | Command::Backup(_, reply)
        | Command::Checkpoint(reply)
        | Command::Close(reply) => {
            let _ = reply.send(Err(anyhow::anyhow!(reason.to_owned())));
        }
    }
}

/// 业务作用：限制原生维护的总等待，不让取消等待提前放开文件权威。
/// 参数说明: 无。
/// 返回：五分钟的完整维护预算。
fn budget() -> nabudget::RequestBudget {
    nabudget::RequestBudget::from_now(Duration::from_secs(300))
}

/// 业务作用：让元数据来源遵守仓库原有 WAL、FULL、外键与空间保留合同。
/// 参数说明：
/// - `root`：规范化仓库目录。
/// - `options`：对象和元数据共享的资源预算。
///
/// 返回：单连接且有界的 NASA SQLite 来源配置。
fn datasource_config(root: &Path, options: &StoreOptions) -> DataSourceConfig {
    DataSourceConfig {
        sqlite: sqlite::SqliteOptions {
            path: root.join("naxus.sqlite3"),
            create_if_missing: true,
            journal_mode: sqlite::JournalMode::Wal,
            synchronous: Some(sqlite::Synchronous::Full),
            foreign_keys: true,
            busy_timeout_ms: options
                .busy_timeout
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            ..Default::default()
        },
        max_connections: 1,
        min_connections: 1,
        min_free_bytes: options.reserve_free_bytes,
        max_result_rows: 1_000_000,
        max_result_bytes: 256 * 1024 * 1024,
        transaction_timeout_ms: 300_000,
        ..Default::default()
    }
}

/// 业务作用：在已取得实例权威的私有目录内收紧旧数据库权限，使旧数据符合 NASA 文件合同。
/// 参数说明：
/// - `root`：当前账号拥有的仓库根。
///
/// 返回：只修改精确数据库及 sidecar 的权限；链接、非普通文件和其它账号文件均拒绝。
pub(crate) fn protect_database(root: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        for name in [
            "naxus.sqlite3",
            "naxus.sqlite3-wal",
            "naxus.sqlite3-shm",
            "naxus.sqlite3-journal",
        ] {
            let path = root.join(name);
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            ensure!(
                metadata.file_type().is_file()
                    && metadata.nlink() == 1
                    && metadata.uid() == unsafe { libc::geteuid() },
                "repository database file ownership is unsafe"
            );
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

/// 业务作用：用 NASA 原生恢复入口把独立 DELETE 备份转为全新 WAL 数据库。
/// 参数说明：
/// - `backup`：已经核对摘要的数据库备份。
/// - `destination`：本次恢复暂存根内不存在的新文件。
///
/// 返回：验证及模式转换后的数据库已持久发布，不覆盖现有文件。
pub(crate) fn restore_database(backup: &Path, destination: &Path) -> Result<()> {
    let runtime =
        tokio::runtime::Handle::try_current().context("restore requires a Tokio runtime")?;
    runtime.block_on(sqlite::restore_to_new(
        backup,
        destination,
        sqlite::JournalMode::Wal,
        &budget(),
    ))?;
    Ok(())
}
