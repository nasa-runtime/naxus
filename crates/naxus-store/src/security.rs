use anyhow::{ensure, Context, Result};
use naxus_core::ArtifactDigest;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Store;

/// 已认证主体，不包含原始口令、会话或访问令牌。
#[derive(Clone, Serialize)]
pub struct AccessIdentity {
    pub subject: String,
    pub username: String,
    pub role: String,
    pub repositories: Vec<String>,
    pub must_change: bool,
    pub can_publish: bool,
    pub auth_version: u64,
}

/// 口令校验专用的受限记录，不序列化到接口。
pub struct LoginUser {
    pub id: String,
    pub password_hash: String,
    pub auth_version: u64,
    pub available: bool,
}

/// 管理界面的用户描述，不暴露任何认证校验材料。
#[derive(Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    pub role: String,
    pub repositories: Vec<String>,
    pub enabled: bool,
    pub must_change: bool,
}

/// 明确授予的 token 能力，首次发布与覆盖既有坐标分别表达。
pub struct TokenGrant<'a> {
    pub label: &'a str,
    pub permission: &'a str,
    pub repositories: &'a [String],
    pub ttl_seconds: u64,
    pub allow_overwrite: bool,
    pub publish_crates: &'a [String],
}

impl Store {
    /// 业务作用：向管理界面提供当前用户可撤销的令牌列表，不返回认证摘要。
    /// 参数说明：
    /// - `actor`：已经认证的浏览器身份。
    ///
    /// 返回：当前用户的有界令牌列表；管理员可看到全域管理描述。
    pub fn tokens(&self, actor: &AccessIdentity) -> Result<Vec<serde_json::Value>> {
        let connection = self.connection()?;
        browser_actor(&connection, actor)?;
        let mut statement = connection.prepare("SELECT id, user_id, label, permission, repositories, expires_at, revoked, allow_overwrite, publish_crates FROM access_tokens WHERE user_id = ?1 OR ?2 = 'admin' ORDER BY expires_at DESC LIMIT 4096")?;
        let rows = statement
            .query_map(params![actor.subject, actor.role], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, bool>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(
                |(
                    id,
                    user_id,
                    label,
                    permission,
                    repositories,
                    expires_at,
                    revoked,
                    allow_overwrite,
                    publish_crates,
                )| {
                    Ok(serde_json::json!({
                        "id": id,
                        "user_id": user_id,
                        "label": label,
                        "permission": permission,
                        "repositories": serde_json::from_str::<Vec<String>>(&repositories)?,
                        "expires_at": expires_at,
                        "revoked": revoked,
                        "allow_overwrite": allow_overwrite,
                        "publish_crates": serde_json::from_str::<Vec<String>>(&publish_crates)?
                    }))
                },
            )
            .collect()
    }

    /// 业务作用：分页读取安全敏感操作审计，避免管理请求加载全部历史。
    /// 参数说明：
    /// - `after`：上次已看到的序号。
    ///
    /// 返回：最多二百条按序号递增的审计，不包含密码或令牌正文；超出持久序号范围的游标按输入拒绝。
    pub fn security_audit(&self, after: u64) -> Result<Vec<serde_json::Value>> {
        ensure!(
            after <= i64::MAX as u64,
            crate::RepositoryFailure::InvalidRequest("invalid audit cursor")
        );
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT sequence, actor, action, target, created_at FROM security_audit WHERE sequence > ?1 ORDER BY sequence LIMIT 200")?;
        let rows = statement
            .query_map([after], |row| {
                Ok(serde_json::json!({
                    "sequence": row.get::<_, u64>(0)?,
                    "actor": row.get::<_, String>(1)?,
                    "action": row.get::<_, String>(2)?,
                    "target": row.get::<_, String>(3)?,
                    "created_at": row.get::<_, i64>(4)?
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 业务作用：从最新记录向前分页展示审计，让长期运行的服务能直接查看刚完成的操作。
    /// 参数说明：
    /// - `before`：上一页最小序号；零表示从最新记录开始。
    ///
    /// 返回：最多二百条降序记录，稳定游标不依赖偏移量；超出持久序号范围的游标按输入拒绝。
    pub fn security_audit_page(&self, before: u64) -> Result<Vec<serde_json::Value>> {
        ensure!(
            before <= i64::MAX as u64,
            crate::RepositoryFailure::InvalidRequest("invalid audit cursor")
        );
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT sequence, actor, action, target, created_at FROM security_audit WHERE ?1 = 0 OR sequence < ?1 ORDER BY sequence DESC LIMIT 200")?;
        let rows = statement
            .query_map([before], |row| {
                Ok(serde_json::json!({
                    "sequence": row.get::<_, u64>(0)?,
                    "actor": row.get::<_, String>(1)?,
                    "action": row.get::<_, String>(2)?,
                    "target": row.get::<_, String>(3)?,
                    "created_at": row.get::<_, i64>(4)?
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 业务作用：修改既有账号角色和仓库范围，并立即撤销先前的会话与 token。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `id`：目标账号 UUID。
    /// - `role`：明确的新角色。
    /// - `repositories`：明确的新仓库 UUID 范围。
    ///
    /// 返回：授权和认证代次一起提交；不能降级最后一个有效管理员。
    pub fn set_user_grants(
        &self,
        actor: &crate::AccessIdentity,
        id: &str,
        role: &str,
        repositories: &[String],
    ) -> Result<()> {
        ensure!(
            matches!(role, "admin" | "operator" | "developer" | "reader")
                && repositories.len() <= 128
                && repositories
                    .iter()
                    .all(|repository| repository == "*" || Uuid::parse_str(repository).is_ok()),
            crate::RepositoryFailure::InvalidRequest("invalid user grants")
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let allowed: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = ?1 AND role = 'admin' AND enabled = 1 AND must_change = 0)", [actor], |row| row.get(0))?;
        ensure!(
            allowed,
            crate::RepositoryFailure::Forbidden("administrator permission required")
        );
        let (old_role, enabled): (String, bool) = transaction.query_row(
            "SELECT role, enabled FROM admin_users WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if old_role == "admin" && enabled && role != "admin" {
            let remaining: u64 = transaction.query_row("SELECT count(*) FROM admin_users WHERE role = 'admin' AND enabled = 1 AND id != ?1", [id], |row| row.get(0))?;
            // 角色修改不能消灭全部管理入口，重启不补建账号或放宽权限。
            ensure!(
                remaining > 0,
                crate::RepositoryFailure::Conflict(
                    "cannot demote the last administrator".to_owned()
                )
            );
        }
        transaction.execute("UPDATE admin_users SET role = ?1, repositories = ?2, auth_version = auth_version + 1 WHERE id = ?3", params![role, serde_json::to_string(repositories)?, id])?;
        transaction.execute("INSERT INTO security_audit(actor, action, target) VALUES (?1, 'change-user-grants', ?2)", params![actor, id])?;
        transaction.commit()?;
        Ok(())
    }

    /// 业务作用：管理员禁用账户时立即使该账户已有会话与令牌失效，同时保留最后一个管理入口。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `id`：目标用户 UUID。
    /// - `enabled`：目标启用状态。
    ///
    /// 返回：启用状态与认证代次同时提交；不能禁用最后一个有效管理员。
    pub fn set_user_enabled(
        &self,
        actor: &crate::AccessIdentity,
        id: &str,
        enabled: bool,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let allowed: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = ?1 AND role = 'admin' AND enabled = 1 AND must_change = 0)", [actor], |row| row.get(0))?;
        ensure!(
            allowed,
            crate::RepositoryFailure::Forbidden("administrator permission required")
        );
        if !enabled {
            let remaining: u64 = transaction.query_row("SELECT count(*) FROM admin_users WHERE role = 'admin' AND enabled = 1 AND id != ?1", [id], |row| row.get(0))?;
            let admin: bool = transaction.query_row(
                "SELECT role = 'admin' FROM admin_users WHERE id = ?1",
                [id],
                |row| row.get(0),
            )?;
            // 账户管理不能消灭全部管理员，否则重启也无法恢复正常管理权。
            ensure!(
                !admin || remaining > 0,
                crate::RepositoryFailure::Conflict(
                    "cannot disable the last administrator".to_owned()
                )
            );
        }
        ensure!(transaction.execute("UPDATE admin_users SET enabled = ?1, auth_version = auth_version + 1 WHERE id = ?2", params![enabled, id])? == 1, crate::RepositoryFailure::NotFound("unknown user"));
        transaction.execute(
            "INSERT INTO security_audit(actor, action, target) VALUES (?1, ?2, ?3)",
            params![
                actor,
                if enabled {
                    "enable_user"
                } else {
                    "disable_user"
                },
                id
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }
    /// 业务作用：判断已有管理员是否需要首启口令，不因进程重启重置账户。
    /// 参数说明: 无。
    ///
    /// 返回：已有用户和仍需更换初始口令的用户数量。
    pub fn bootstrap_status(&self) -> Result<(u64, u64)> {
        Ok(self.connection()?.query_row(
            "SELECT count(*), coalesce(sum(must_change), 0) FROM admin_users",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    /// 业务作用：在空数据卷建立唯一初始管理员，首次登录只能设置新口令。
    /// 参数说明：
    /// - `password_hash`：由 NASA BCrypt 生成的口令校验材料。
    ///
    /// 返回：空卷创建成功；已有用户时保持原状态。
    pub fn bootstrap_admin(&self, password_hash: &str) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: u64 =
            transaction.query_row("SELECT count(*) FROM admin_users", [], |row| row.get(0))?;
        if count == 0 {
            let id = Uuid::new_v4().to_string();
            transaction.execute("INSERT INTO admin_users(id, username, password_hash, role, repositories, must_change) VALUES (?1, 'admin', ?2, 'admin', ?3, 1)", params![id, password_hash, serde_json::to_string(&["*"])?])?;
            transaction.execute("INSERT INTO security_audit(actor, action, target) VALUES ('bootstrap', 'create_administrator', ?1)", [&id])?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// 业务作用：读取口令校验所需记录，退避中的账户仍进行同样的外部哈希工作。
    /// 参数说明：
    /// - `username`：已经过长度和字符门禁的用户名。
    ///
    /// 返回：存在用户的受限校验材料；不存在为 None。
    pub fn login_user(&self, username: &str) -> Result<Option<LoginUser>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT id, password_hash, auth_version, enabled = 1 AND not_before <= unixepoch()
             FROM admin_users WHERE username = ?1",
                [username],
                |row| {
                    Ok(LoginUser {
                        id: row.get(0)?,
                        password_hash: row.get(1)?,
                        auth_version: row.get(2)?,
                        available: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// 业务作用：在口令校验后复验账户代次并创建会话，防止并发禁用或换密后沿用旧验证。
    /// 参数说明：
    /// - `user`：本次口令校验时的账户记录。
    /// - `accepted`：哈希与账户可用性共同判断结果。
    /// - `session`：浏览器会话的 SHA-256。
    /// - `csrf`：CSRF 材料的 SHA-256。
    ///
    /// 返回：认证成功才持久化八小时会话；失败记录有界退避并返回 false。
    pub fn finish_login(
        &self,
        user: &LoginUser,
        accepted: bool,
        session: &ArtifactDigest,
        csrf: &ArtifactDigest,
    ) -> Result<bool> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM admin_sessions WHERE digest IN (SELECT digest FROM admin_sessions WHERE expires_at <= unixepoch() LIMIT 256)", [])?;
        let current: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = ?1 AND auth_version = ?2 AND enabled = 1 AND not_before <= unixepoch())", params![user.id, user.auth_version], |row| row.get(0))?;
        let accepted = accepted && current;
        if accepted {
            let (total, owned): (u64, u64) = transaction.query_row(
                "SELECT count(*), coalesce(sum(user_id = ?1), 0) FROM admin_sessions",
                [&user.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            ensure!(
                total < 4096 && owned < 32,
                crate::ResourceLimitExceeded("session budget exceeded")
            );
            transaction.execute(
                "UPDATE admin_users SET failures = 0, not_before = 0 WHERE id = ?1",
                [&user.id],
            )?;
            transaction.execute("INSERT INTO admin_sessions(digest, user_id, csrf_digest, auth_version, expires_at) VALUES (?1, ?2, ?3, ?4, unixepoch() + 28800)", params![session.as_str(), user.id, csrf.as_str(), user.auth_version])?;
        } else {
            transaction.execute("UPDATE admin_users SET failures = min(failures + 1, 15), not_before = CASE WHEN failures >= 4 THEN unixepoch() + min(900, (1 << min(failures - 3, 10))) ELSE not_before END WHERE id = ?1 AND auth_version = ?2", params![user.id, user.auth_version])?;
        }
        transaction.execute(
            "INSERT INTO security_audit(actor, action, target) VALUES (?1, ?2, ?1)",
            params![
                user.id,
                if accepted {
                    "login"
                } else {
                    "login_rejected"
                }
            ],
        )?;
        transaction.commit()?;
        Ok(accepted)
    }

    /// 业务作用：复验浏览器会话、账户状态和认证代次，并对写请求核对 CSRF 摘要。
    /// 参数说明：
    /// - `session`：Cookie 材料的摘要。
    /// - `csrf`：写请求显式提交的 CSRF 摘要；只读请求为 None。
    ///
    /// 返回：合法会话的当前身份，撤销、禁用、过期或 CSRF 不符为 None。
    pub fn browser_identity(
        &self,
        session: &ArtifactDigest,
        csrf: Option<&ArtifactDigest>,
    ) -> Result<Option<AccessIdentity>> {
        let raw: Option<(String, String, String, String, bool, u64)> = self.connection()?.query_row("SELECT u.id, u.username, u.role, u.repositories, u.must_change, u.auth_version FROM admin_sessions s JOIN admin_users u ON u.id = s.user_id WHERE s.digest = ?1 AND s.expires_at > unixepoch() AND s.auth_version = u.auth_version AND u.enabled = 1 AND (?2 IS NULL OR s.csrf_digest = ?2)", params![session.as_str(), csrf.map(ArtifactDigest::as_str)], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))).optional()?;
        raw.map(
            |(subject, username, role, repositories, must_change, auth_version)| {
                Ok(AccessIdentity {
                    can_publish: role != "reader",
                    subject,
                    username,
                    role,
                    repositories: serde_json::from_str(&repositories)?,
                    must_change,
                    auth_version,
                })
            },
        )
        .transpose()
    }

    /// 业务作用：明确注销浏览器会话，不影响其他设备的独立会话。
    /// 参数说明：
    /// - `session`：当前会话摘要。
    ///
    /// 返回：会话不再可用；重复注销保持幂等。
    pub fn logout(&self, session: &ArtifactDigest) -> Result<()> {
        self.connection()?.execute(
            "DELETE FROM admin_sessions WHERE digest = ?1",
            [session.as_str()],
        )?;
        Ok(())
    }

    /// 业务作用：读取有权限的管理用户列表，不返回哈希或访问令牌正文。
    /// 参数说明: 无。
    ///
    /// 返回：有界且按用户名排序的账户描述。
    pub fn users(&self) -> Result<Vec<UserInfo>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT id, username, role, repositories, enabled, must_change FROM admin_users ORDER BY username LIMIT 4096")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, username, role, repositories, enabled, must_change)| {
                Ok(UserInfo {
                    id,
                    username,
                    role,
                    repositories: serde_json::from_str(&repositories)?,
                    enabled,
                    must_change,
                })
            })
            .collect()
    }

    /// 业务作用：由管理员创建具有明确仓库边界的用户，不把口令明文写入审计。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `username`：新用户名称。
    /// - `hash`：NASA 生成的口令哈希。
    /// - `role`：admin/operator/developer/reader。
    /// - `repositories`：允许的仓库 UUID 或全域标记。
    ///
    /// 返回：新建用户 UUID；非法输入、重复名称、越权与预算不足分别保留领域拒绝，不写入部分账户或审计。
    pub fn create_user(
        &self,
        actor: &crate::AccessIdentity,
        username: &str,
        hash: &str,
        role: &str,
        repositories: &[String],
    ) -> Result<String> {
        naxus_core::validate_name(username)
            .context(crate::RepositoryFailure::InvalidRequest("invalid username"))?;
        ensure!(
            matches!(role, "admin" | "operator" | "developer" | "reader")
                && repositories.len() <= 128,
            crate::RepositoryFailure::InvalidRequest("invalid user grants")
        );
        for repository in repositories {
            ensure!(
                repository == "*" || Uuid::parse_str(repository).is_ok(),
                crate::RepositoryFailure::InvalidRequest("invalid repository grant")
            );
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let admin: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = ?1 AND role = 'admin' AND enabled = 1 AND must_change = 0)", [actor], |row| row.get(0))?;
        ensure!(
            admin,
            crate::RepositoryFailure::Forbidden("administrator permission required")
        );
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM admin_users WHERE username = ?1)",
            [username],
            |row| row.get(0),
        )?;
        // 写事务内先确认名称可用，并发创建也不能把确定性账户冲突归为存储故障。
        ensure!(
            !exists,
            crate::RepositoryFailure::Conflict("username already exists".to_owned())
        );
        let count: u64 =
            transaction.query_row("SELECT count(*) FROM admin_users", [], |row| row.get(0))?;
        ensure!(
            count < 4096,
            crate::ResourceLimitExceeded("user budget exceeded")
        );
        let id = Uuid::new_v4().to_string();
        transaction.execute("INSERT INTO admin_users(id, username, password_hash, role, repositories) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, username, hash, role, serde_json::to_string(repositories)?])?;
        transaction.execute(
            "INSERT INTO security_audit(actor, action, target) VALUES (?1, 'create_user', ?2)",
            params![actor, id],
        )?;
        transaction.commit()?;
        Ok(id)
    }

    /// 业务作用：换密时撤销该用户的全部会话与访问令牌，防止旧凭据继续发布。
    /// 参数说明：
    /// - `user`：已验证旧口令的账户记录。
    /// - `hash`：新口令哈希。
    ///
    /// 返回：认证代次改变且旧凭据失效；并发换密或禁用时拒绝。
    pub fn change_password(&self, user: &LoginUser, hash: &str) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute("UPDATE admin_users SET password_hash = ?1, must_change = 0, auth_version = auth_version + 1, failures = 0, not_before = 0 WHERE id = ?2 AND auth_version = ?3 AND enabled = 1", params![hash, user.id, user.auth_version])?;
        ensure!(
            changed == 1,
            crate::RepositoryFailure::Conflict("account authentication changed".to_owned())
        );
        transaction.execute("DELETE FROM admin_sessions WHERE user_id = ?1", [&user.id])?;
        transaction.execute(
            "INSERT INTO security_audit(actor, action, target) VALUES (?1, 'change_password', ?1)",
            [&user.id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// 业务作用：将高熵访问令牌绑定用户、权限与仓库范围，明文只由调用方展示一次。
    /// 参数说明：
    /// - `actor`：已经登录且不处于初始换密状态的用户。
    /// - `digest`：令牌 SHA-256。
    /// - `label`：可识别用途的非敏感标签。
    /// - `permission`：read 或 publish。
    /// - `repositories`：明确缩小后的仓库范围。
    /// - `ttl_seconds`：有效期限，最多一年。
    ///
    /// 返回：令牌管理 UUID；越权、非法期限或预算不足拒绝。
    pub fn create_token(
        &self,
        actor: &AccessIdentity,
        digest: &ArtifactDigest,
        label: &str,
        permission: &str,
        repositories: &[String],
        ttl_seconds: u64,
    ) -> Result<String> {
        self.create_scoped_token(
            actor,
            digest,
            &TokenGrant {
                label,
                permission,
                repositories,
                ttl_seconds,
                allow_overwrite: false,
                publish_crates: &[],
            },
        )
    }

    /// 业务作用：生成限仓库、可选精确包名且显式区分覆盖权的独立 token。
    /// 参数说明：
    /// - `actor`：当前仍有效的浏览器身份。
    /// - `digest`：高熵 token 的校验摘要。
    /// - `grant`：用途、期限、仓库及发布能力。
    ///
    /// 返回：仅保存摘要与明确授权；非法能力组合按输入拒绝，超过当前账号范围按授权拒绝，预算不足不创建令牌。
    pub fn create_scoped_token(
        &self,
        actor: &AccessIdentity,
        digest: &ArtifactDigest,
        grant: &TokenGrant<'_>,
    ) -> Result<String> {
        let TokenGrant {
            label,
            permission,
            repositories,
            ttl_seconds,
            allow_overwrite,
            publish_crates,
        } = *grant;
        ensure!(
            matches!(permission, "read" | "publish"),
            crate::RepositoryFailure::InvalidRequest("invalid token permission")
        );
        ensure!(
            (!allow_overwrite && publish_crates.is_empty()) || permission == "publish",
            crate::RepositoryFailure::InvalidRequest(
                "read token cannot grant publishing capabilities"
            )
        );
        ensure!(
            publish_crates.len() <= 128
                && publish_crates
                    .iter()
                    .all(|name| naxus_core::validate_name(name).is_ok()),
            crate::RepositoryFailure::InvalidRequest("invalid crate publish scope")
        );
        ensure!(
            repositories
                .iter()
                .all(|repository| repository == "*" || Uuid::parse_str(repository).is_ok()),
            crate::RepositoryFailure::InvalidRequest("invalid token repository scope")
        );
        ensure!(
            !actor.must_change && (permission != "publish" || actor.can_publish),
            crate::RepositoryFailure::Forbidden("token permission denied")
        );
        ensure!(
            !label.is_empty()
                && label.len() <= 128
                && !label.chars().any(char::is_control)
                && (60..=31_536_000).contains(&ttl_seconds)
                && !repositories.is_empty()
                && repositories.len() <= 128,
            crate::RepositoryFailure::InvalidRequest("invalid token parameters")
        );
        // token 只能缩小当前账户的仓库范围；拒绝扩大授权时不创建凭据或审计记录。
        ensure!(
            repositories
                .iter()
                .all(|repository| actor.repositories.contains(&"*".to_owned())
                    || actor.repositories.contains(repository)),
            crate::RepositoryFailure::Forbidden("token repository scope exceeds user grant")
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 换密或授权变化后的旧请求不能借用新代次签发永久凭据，必须复验入站身份的同一代次。
        browser_actor(&transaction, actor)?;
        let version = actor.auth_version;
        let count: u64 = transaction.query_row("SELECT count(*) FROM access_tokens WHERE user_id = ?1 AND revoked = 0 AND expires_at > unixepoch()", [&actor.subject], |row| row.get(0))?;
        ensure!(
            count < 128,
            crate::ResourceLimitExceeded("token budget exceeded")
        );
        let id = Uuid::new_v4().to_string();
        transaction.execute("INSERT INTO access_tokens(id, digest, user_id, label, permission, repositories, auth_version, expires_at, allow_overwrite, publish_crates) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, unixepoch() + ?8, ?9, ?10)", params![id, digest.as_str(), actor.subject, label, permission, serde_json::to_string(repositories)?, version, ttl_seconds, allow_overwrite, serde_json::to_string(publish_crates)?])?;
        transaction.execute(
            "INSERT INTO security_audit(actor, action, target) VALUES (?1, 'create_token', ?2)",
            params![actor.subject, id],
        )?;
        transaction.commit()?;
        Ok(id)
    }

    /// 业务作用：为 Cargo 验证独立访问令牌，并按当前用户状态收窄其仓库权限。
    /// 参数说明：
    /// - `digest`：请求令牌的 SHA-256。
    ///
    /// 返回：未撤销、未过期且账户代次匹配的最小身份；其余情况为 None。
    pub fn token_identity(&self, digest: &ArtifactDigest) -> Result<Option<AccessIdentity>> {
        let connection = self.connection()?;
        token_identity_on(&connection, digest)
    }

    /// 业务作用：撤销当前用户持有的访问令牌，管理员可以撤销其他用户的令牌。
    /// 参数说明：
    /// - `actor`：当前已认证浏览器身份。
    /// - `id`：令牌管理 UUID。
    ///
    /// 返回：受权令牌被撤销，越权不改变状态。
    pub fn revoke_token(&self, actor: &AccessIdentity, id: &str) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        browser_actor(&transaction, actor)?;
        let owner: String = transaction
            .query_row(
                "SELECT user_id FROM access_tokens WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .context(crate::RepositoryFailure::NotFound("unknown token"))?;
        ensure!(
            owner == actor.subject || actor.role == "admin",
            crate::RepositoryFailure::Forbidden("token ownership permission denied")
        );
        transaction.execute("UPDATE access_tokens SET revoked = 1 WHERE id = ?1", [id])?;
        transaction.execute(
            "INSERT INTO security_audit(actor, action, target) VALUES (?1, 'revoke_token', ?2)",
            params![actor.subject, id],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

/// 业务作用：在管理读写所用的数据库视图内复验账号代次与授权快照，阻止旧请求跨越换密或授权变更。
/// 参数说明：
/// - `connection`：当前管理事务或受锁保护的读取连接。
/// - `actor`：请求认证时取得的账号与授权状态。
///
/// 返回：同代次、启用且授权一致时提供审计主体 UUID；账号变化或尚未换密时拒绝后续副作用。
pub(crate) fn browser_actor<'a>(
    connection: &rusqlite::Connection,
    actor: &'a AccessIdentity,
) -> Result<&'a str> {
    let current: Option<(String, String)> = connection.query_row(
        "SELECT role, repositories FROM admin_users WHERE id = ?1 AND auth_version = ?2 AND enabled = 1 AND must_change = 0",
        params![actor.subject, actor.auth_version],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    // 入站认证之后仍可能等待队列或口令计算；提交事务不能借用账号的新代次来完成旧请求。
    let (role, repositories) = current.context(crate::RepositoryFailure::Forbidden(
        "browser identity permission revoked",
    ))?;
    ensure!(
        !actor.must_change
            && role == actor.role
            && actor.can_publish == (role != "reader")
            && serde_json::from_str::<Vec<String>>(&repositories)? == actor.repositories,
        crate::RepositoryFailure::Forbidden("browser identity permission changed")
    );
    Ok(&actor.subject)
}

/// 业务作用：在调用方已有事务内复验 token 与账号代次，允许写入提交和授权使用同一数据库视图。
/// 参数说明：
/// - `connection`：当前查询连接或提交事务。
/// - `digest`：已提取的令牌摘要。
///
/// 返回：当前有效身份和收窄后的仓库范围，失效时为 None。
pub(crate) fn token_identity_on(
    connection: &rusqlite::Connection,
    digest: &ArtifactDigest,
) -> Result<Option<AccessIdentity>> {
    let raw: Option<(String, String, String, String, String, bool, u64)> = connection.query_row("SELECT u.id, u.username, u.role, u.repositories, t.repositories, t.permission = 'publish', u.auth_version FROM access_tokens t JOIN admin_users u ON u.id = t.user_id WHERE t.digest = ?1 AND t.revoked = 0 AND t.expires_at > unixepoch() AND t.auth_version = u.auth_version AND u.enabled = 1 AND u.must_change = 0", [digest.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))).optional()?;
    raw.map(
        |(subject, username, role, granted, scopes, publish, auth_version)| {
            let granted: Vec<String> = serde_json::from_str(&granted)?;
            let scopes: Vec<String> = serde_json::from_str(&scopes)?;
            let repositories = scopes
                .into_iter()
                .filter(|scope| granted.contains(&"*".to_owned()) || granted.contains(scope))
                .collect();
            Ok(AccessIdentity {
                can_publish: publish && role != "reader",
                subject,
                username,
                role,
                repositories,
                must_change: false,
                auth_version,
            })
        },
    )
    .transpose()
}
