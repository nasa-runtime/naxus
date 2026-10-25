"use strict";

let currentUser = null;
let currentView = "repositories";
let selectedRepository = "";
let packageCursor = "";
let packageName = "";
let selectedProxy = "";
let proxyCursor = "";
let taskRefresh = null;
let selectedPinRepository = "";
let editingUser = null;
let auditBefore = 0;
let sessionGeneration = 0;
const pendingRequests = new Set();

/**
 * 业务作用：保留管理请求的协议分类和关联 ID，界面不解析自然语言来判断身份是否失效。
 * 参数说明：
 * - status：HTTP 状态，零表示网络或本地请求状态。
 * - code：服务端或客户端的固定失败类别。
 * - message：可展示的安全提示。
 * - requestId：服务端生成的请求关联 ID。
 * 返回：可区分认证、权限、依赖与已取消请求的错误。
 */
class AdminApiError extends Error {
  /**
   * 业务作用：建立一次管理请求的失败证据，不保存请求正文或认证材料。
   * 参数说明：
   * - status：真实 HTTP 状态或网络失败标记。
   * - code：稳定错误类别。
   * - message：安全的界面说明。
   * - requestId：可选服务端关联 ID。
   * 返回：仅包含错误协议字段的对象。
   */
  constructor(status, code, message, requestId) {
    super(message);
    this.name = "AdminApiError";
    this.status = status;
    this.code = code;
    this.requestId = requestId;
    this.sessionExpired = false;
  }
}

/**
 * 业务作用：撤销当前浏览器身份的在途工作，避免旧页面响应在重新登录后继续发布内容。
 * 参数说明: 无。
 * 返回：请求与轮询停止，身份、选择项、账户区和导航均清空。
 */
function clearIdentity() {
  // 先关闭旧请求的发布资格，再清理界面；已经接收响应的旧请求也必须因代际不符而停止。
  sessionGeneration += 1;
  for (const controller of pendingRequests) {
    controller.abort();
  }
  pendingRequests.clear();
  if (taskRefresh !== null) {
    clearTimeout(taskRefresh);
    taskRefresh = null;
  }
  currentUser = null;
  currentView = "login";
  selectedRepository = "";
  packageCursor = "";
  packageName = "";
  selectedProxy = "";
  proxyCursor = "";
  selectedPinRepository = "";
  editingUser = null;
  auditBefore = 0;
  document.querySelector("#account").replaceChildren();
  document.querySelector("#navigation").replaceChildren();
}

/**
 * 业务作用：将服务端 Unix 秒转换为浏览器本地时间，避免管理人员手动换算期限。
 * 参数说明：
 * - seconds：服务端时间戳。
 * 返回：可读时间，缺失或无效值显示占位。
 */
function localTime(seconds) {
  const date = new Date(Number(seconds) * 1000);
  return Number.isFinite(date.getTime()) ? date.toLocaleString() : "—";
}

/**
 * 业务作用：创建只使用文本内容的页面节点，服务数据不会解释为 HTML。
 * 参数说明：
 * - tag：标准元素名。
 * - text：可选纯文本。
 * - className：固定样式类。
 * 返回：尚未挂载的 DOM 元素。
 */
function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) {
    node.textContent = text;
  }
  if (className) {
    node.className = className;
  }
  return node;
}

/**
 * 业务作用：从独立 Cookie 读取 CSRF 材料，认证 Cookie 始终保持 HttpOnly。
 * 参数说明: 无。
 * 返回：CSRF 材料或空字符串；不保存密码和 Cargo token。
 */
function csrf() {
  const cookie = document.cookie
    .split(";")
    .map((part) => part.trim())
    .find((part) => part.startsWith("naxus_csrf="));
  return cookie ? cookie.slice("naxus_csrf=".length) : "";
}

/**
 * 业务作用：通过同源管理接口请求数据，为每个写操作附加 CSRF。
 * 参数说明：
 * - path：管理接口的相对路径。
 * - method：HTTP 方法。
 * - value：可选 JSON 请求。
 * 返回：当前身份代际的成功正文；401 立即回登录态，其他失败保留状态、错误码及请求 ID。
 */
async function api(path, method = "GET", value) {
  const generation = sessionGeneration;
  const controller = new AbortController();
  pendingRequests.add(controller);
  const options = {
    method,
    credentials: "same-origin",
    headers: {},
    cache: "no-store",
    signal: controller.signal,
  };
  if (method !== "GET") {
    options.headers["X-CSRF-Token"] = csrf();
  }
  if (value !== undefined) {
    options.headers["Content-Type"] = "application/json";
    options.body = JSON.stringify(value);
  }
  try {
    const response = await fetch(
      new URL(`./api/v1/${path}`, window.location.href),
      options,
    );
    let body = null;
    if (response.status !== 204) {
      try {
        body = await response.json();
      } catch (_) {
        if (response.ok) {
          throw new AdminApiError(
            response.status,
            "invalid_response",
            "仓库服务返回了无效响应。",
            response.headers.get("x-request-id"),
          );
        }
      }
    }
    if (generation !== sessionGeneration) {
      throw new AdminApiError(
        0,
        "request_cancelled",
        "请求所属会话已关闭。",
        null,
      );
    }
    if (!response.ok) {
      const error = new AdminApiError(
        response.status,
        typeof body?.code === "string" ? body.code : "http_error",
        typeof body?.message === "string"
          ? body.message
          : `HTTP ${response.status}`,
        body?.request_id || response.headers.get("x-request-id"),
      );
      if (response.status === 401) {
        error.sessionExpired = currentUser !== null;
        loginPage();
      }
      throw error;
    }
    return body;
  } catch (error) {
    const authenticationFailure =
      error instanceof AdminApiError && error.status === 401;
    if (
      (controller.signal.aborted || generation !== sessionGeneration) &&
      !authenticationFailure
    ) {
      throw new AdminApiError(
        0,
        "request_cancelled",
        "请求所属会话已关闭。",
        null,
      );
    }
    if (error instanceof AdminApiError) {
      throw error;
    }
    throw new AdminApiError(
      0,
      "network_unavailable",
      "无法连接仓库服务，请稍后重试。",
      null,
    );
  } finally {
    pendingRequests.delete(controller);
  }
}

/**
 * 业务作用：展示当前管理失败及请求 ID，旧会话取消不会覆盖新页面的通知。
 * 参数说明：
 * - error：安全错误描述；为空时清除通知。
 * 返回：当前错误更新通知；已取消请求保持原通知，不输出认证材料。
 */
function notify(error) {
  if (error?.code === "request_cancelled" || error?.name === "AbortError") {
    return;
  }
  let message = error ? error.message : "";
  if (error instanceof AdminApiError && error.status === 401) {
    message =
      error.code === "invalid_credentials"
        ? "用户名或口令不正确。"
        : error.sessionExpired
          ? "会话已失效，请重新登录。"
          : "请登录仓库控制台。";
  }
  if (error?.requestId) {
    message += `（请求 ID：${error.requestId}）`;
  }
  document.querySelector("#notice").textContent = message;
}

/**
 * 业务作用：生成具有明确输入语义和浏览器自动填充边界的表单控件。
 * 参数说明：
 * - form：所属表单。
 * - label：显示名称。
 * - name：提交字段。
 * - type：输入类型。
 * - value：默认文本。
 * 返回：新增的输入元素。
 */
function input(form, label, name, type = "text", value = "") {
  const wrapper = element("label", label);
  const control = element("input");
  control.name = name;
  control.type = type;
  control.value = value;
  control.required = true;
  control.autocomplete = type === "password" ? "new-password" : "off";
  wrapper.append(control);
  form.append(wrapper);
  return control;
}

/**
 * 业务作用：创建单次提交的管理表单，执行期间关闭重复点击。
 * 参数说明：
 * - parent：挂载目标。
 * - label：提交按钮文本。
 * - operation：接收表单值的异步业务操作。
 * 返回：可继续增加输入项的表单元素。
 */
function form(parent, label, operation) {
  const node = element("form");
  const button = element("button", label, "primary");
  button.type = "submit";
  node.addEventListener("submit", async (event) => {
    event.preventDefault();
    notify(null);
    button.disabled = true;
    try {
      await operation(Object.fromEntries(new FormData(node)));
    } catch (error) {
      notify(error);
    } finally {
      button.disabled = false;
    }
  });
  node.append(button);
  parent.append(node);
  return node;
}

/**
 * 业务作用：完成表单布局，使按钮位于所有字段之后。
 * 参数说明：
 * - node：已经配置输入项的表单。
 * 返回：提交按钮移至末尾。
 */
function finishForm(node) {
  node.append(node.querySelector("button[type=submit]"));
}

/**
 * 业务作用：更新管理页面标题和可读说明。
 * 参数说明：
 * - title：当前业务页面名。
 * - description：当前操作边界。
 * 返回：可挂载内容的页面根。
 */
function heading(title, description) {
  const content = document.querySelector("#content");
  content.replaceChildren(element("h1", title), element("p", description));
  return content;
}

/**
 * 业务作用：以纯文本表格展示管理对象，避免制品名称或标签引入脚本。
 * 参数说明：
 * - parent：挂载目标。
 * - columns：显示标题与字段映射。
 * - rows：管理对象列表。
 * - action：可选的逐行操作节点生成器。
 * 返回：生成只读表格及授权操作。
 */
function table(parent, columns, rows, action) {
  const panel = element("div", undefined, "panel");
  const node = element("table");
  const head = element("tr");
  for (const [title] of columns) {
    head.append(element("th", title));
  }
  if (action) {
    head.append(element("th", "操作"));
  }
  node.append(head);
  for (const row of rows) {
    const line = element("tr");
    for (const [, key] of columns) {
      const value = row[key];
      line.append(
        element(
          "td",
          Array.isArray(value) ? value.join(", ") : String(value ?? "—"),
        ),
      );
    }
    if (action) {
      const cell = element("td");
      const control = action(row);
      if (control) {
        cell.append(control);
      }
      line.append(cell);
    }
    node.append(line);
  }
  if (rows.length === 0) {
    panel.append(element("p", "当前没有记录。"));
  } else {
    panel.append(node);
  }
  parent.append(panel);
}

/**
 * 业务作用：执行一次有界管理动作并重新读取页面，不提前显示成功。
 * 参数说明：
 * - label：按钮业务含义。
 * - operation：提交操作。
 * - refresh：完成后是否重新读取当前页面。
 * 返回：绑定操作的按钮。
 */
function actionButton(label, operation, refresh = true) {
  const button = element("button", label);
  button.addEventListener("click", async () => {
    button.disabled = true;
    notify(null);
    try {
      await operation();
      if (refresh) {
        await show(currentView);
      }
    } catch (error) {
      notify(error);
    } finally {
      button.disabled = false;
    }
  });
  return button;
}

/**
 * 业务作用：建立登录页面，认证成功后从服务端重新读取当前身份。
 * 参数说明: 无。
 * 返回：登录表单，不保留口令到浏览器存储。
 */
function loginPage() {
  clearIdentity();
  const content = heading(
    "欢迎使用 Naxus",
    "登录后管理仓库、用户与团队访问凭据。",
  );
  const panel = element("div", undefined, "panel login");
  panel.append(element("h2", "登录仓库控制台"));
  const node = form(panel, "登录", async (values) => {
    await api("login", "POST", values);
    node.reset();
    await start();
  });
  input(node, "用户名", "username").autocomplete = "username";
  input(node, "口令", "password", "password").autocomplete = "current-password";
  finishForm(node);
  panel.append(
    element(
      "p",
      "首次启动的管理员口令位于数据目录 bootstrap/admin.password，首次登录须设置新口令。",
    ),
  );
  content.append(panel);
}

/**
 * 业务作用：提供换密入口，初始管理员只能完成此步骤后使用其他管理能力。
 * 参数说明: 无。
 * 返回：换密成功后清除本页口令并重新登录。
 */
function passwordPage() {
  const content = heading(
    currentUser.must_change ? "设置管理员口令" : "更换口令",
    "新口令须为 12 至 72 个 UTF-8 字节。换密会使当前用户全部会话和 Cargo token 失效。",
  );
  const panel = element("div", undefined, "panel login");
  const node = form(panel, "设置口令并重新登录", async (values) => {
    if (values.new_password !== values.confirm_password) {
      throw new Error("两次新口令不一致。");
    }
    await api("password", "POST", {
      current_password: values.current_password,
      new_password: values.new_password,
    });
    node.reset();
    currentUser = null;
    loginPage();
  });
  input(node, "当前口令", "current_password", "password").autocomplete =
    "current-password";
  input(node, "新口令", "new_password", "password");
  input(node, "确认新口令", "confirm_password", "password");
  finishForm(node);
  content.append(panel);
}

/**
 * 业务作用：展示真实仓库来源与不可变身份，避免混淆 hosted 与公开代理。
 * 参数说明: 无。
 * 返回：当前主体有权查看的来源概览。
 */
async function repositoryPage() {
  const data = await api("repositories");
  const content = heading(
    "仓库总览",
    "一个服务地址，连接私有制品和独立公开来源。",
  );
  const cards = element("div", undefined, "cards");
  for (const [label, count] of [
    ["Hosted 仓库", data.hosted.length],
    ["公开代理", data.proxies.length],
  ]) {
    const card = element("div", label, "panel");
    card.append(element("div", String(count), "number"));
    cards.append(card);
  }
  content.append(cards);
  if (data.storage) {
    content.append(
      element(
        "p",
        `数据卷可用 ${(data.storage.free_bytes / 1073741824).toFixed(2)} GiB · 写入${data.storage.writable ? "可用" : "已达到保留空间门限，已有包仍可读取"}`,
      ),
    );
  }
  if (data.proxy_metrics) {
    content.append(
      element(
        "p",
        `公开包体缓存命中 ${data.proxy_metrics.artifact_cache_hits} 次 · 实际回源包体 ${data.proxy_metrics.upstream_artifact_requests} 次 / ${data.proxy_metrics.upstream_artifact_bytes} 字节。索引回源 ${data.proxy_metrics.upstream_index_requests} 次单独统计。`,
      ),
    );
  }
  table(
    content,
    [
      ["HOSTED", "name"],
      ["策略", "policy"],
      ["仓库 UUID", "id"],
      ["当前快照", "head"],
    ],
    data.hosted,
  );
  table(
    content,
    [
      ["PROXY", "name"],
      ["来源 UUID", "id"],
      ["逻辑来源", "logical_index"],
    ],
    data.proxies,
  );
  content.append(element("p", `服务 UUID · ${data.service_uuid}`, "identity"));
}

/**
 * 业务作用：在提交管理动作之前显示服务端固定的坐标、HEAD 和历史保留影响。
 * 参数说明：
 * - action：精确仓库、版本范围和管理操作。
 * 返回：确认区域；只有点击确认后才提交新的仓库快照。
 */
async function previewAction(action) {
  const preview = await api("hosted/previews", "POST", action);
  const panel = element("div", undefined, "panel");
  panel.append(element("h2", "确认管理范围"));
  panel.append(
    element("p", `操作 ${preview.operation} · 仓库 ${preview.repository_id}`),
  );
  panel.append(element("p", `预期 HEAD：${preview.expected_head}`, "identity"));
  panel.append(
    element(
      "p",
      "此操作保留历史快照和对象，不立即释放物理存储。预览后仓库发生发布时，提交会被拒绝并要求重新预览。",
    ),
  );
  const list = element("ul");
  for (const item of preview.coordinates) {
    list.append(element("li", `${item.name} @ ${item.version}`));
  }
  panel.append(list);
  const key = crypto.randomUUID();
  panel.append(
    actionButton("确认提交此范围", () =>
      api(`hosted/previews/${encodeURIComponent(preview.id)}/commit`, "POST", {
        fingerprint: preview.fingerprint,
        idempotency_key: key,
      }),
    ),
  );
  panel.append(actionButton("取消", async () => {}));
  document.querySelector("#content").append(panel);
  panel.scrollIntoView({ behavior: "smooth", block: "start" });
}

/**
 * 业务作用：查询精确坐标的不可变修订，为获授权用户提供回退预览入口。
 * 参数说明：
 * - row：已列出的精确坐标。
 * - before：上一页返回的排他游标，零表示最新记录。
 * 返回：一页历史及更早页入口，回退仍需独立预览和确认。
 */
async function historyPage(row, before = 0) {
  const path = `repositories/${encodeURIComponent(selectedRepository)}/packages/${encodeURIComponent(row.name)}/${encodeURIComponent(row.version)}/revisions`;
  const page = await api(`${path}?before=${before}`);
  const panel = element("div", undefined, "panel");
  panel.id = "revision-history";
  panel.append(element("h2", `${row.name} @ ${row.version} · 修订历史`));
  const mutable = ["admin", "operator"].includes(currentUser.role);
  table(
    panel,
    [
      ["修订 UUID", "id"],
      ["对象 SHA-256", "sha256"],
      ["发布主体", "actor"],
    ],
    page.items,
    mutable
      ? (revision) => {
          const actions = element("div");
          actions.append(
            actionButton(
              "预览回退",
              () =>
                previewAction({
                  repository: selectedRepository,
                  name: row.name,
                  version: row.version,
                  operation: "rollback",
                  revision: revision.id,
                }),
              false,
            ),
          );
          actions.append(
            actionButton(
              "物理清除预览",
              () => hostedClearSelection(row, revision),
              false,
            ),
          );
          return actions;
        }
      : null,
  );
  if (before) {
    panel.append(actionButton("最新修订", () => historyPage(row), false));
  }
  if (page.next !== null) {
    panel.append(
      actionButton("更早修订", () => historyPage(row, page.next), false),
    );
  }
  const previous = document.querySelector("#revision-history");
  if (previous) {
    previous.replaceWith(panel);
  } else {
    document.querySelector("#content").append(panel);
  }
  panel.scrollIntoView({ behavior: "smooth", block: "start" });
}

/**
 * 业务作用：要求显式选择是否撤销历史访问，再生成包含活动保护的物理清除预览。
 * 参数说明：
 * - row：精确包与版本。
 * - revision：用户选中的历史修订。
 * 返回：默认不撤销历史的范围选择，尚未提交清除。
 */
function hostedClearSelection(row, revision) {
  const panel = element("div", undefined, "panel");
  panel.append(
    element("h2", "历史修订清除范围"),
    element("p", `${row.name} @ ${row.version} · ${revision.id}`),
  );
  panel.append(
    element(
      "p",
      "清除后该历史修订可能无法再次下载。新发布的同版本修订不能冒充原内容；当前 HEAD、活动租约、保留点和在途读取仍受保护。",
    ),
  );
  const node = form(panel, "计算清除影响", async () => {
    await physicalPreview({
      kind: "hosted",
      repository: selectedRepository,
      name: row.name,
      version: row.version,
      revision: revision.id,
      revoke_history: confirmation.checked,
    });
  });
  const label = element("label", "明确撤销此历史修订的读取保留");
  const confirmation = element("input");
  confirmation.type = "checkbox";
  confirmation.name = "revoke_history";
  label.append(confirmation);
  node.append(label);
  finishForm(node);
  document.querySelector("#content").append(panel);
  panel.scrollIntoView({ behavior: "smooth", block: "start" });
}

/**
 * 业务作用：显示服务端固定的对象代次、保护阻挡及真实可回收空间，确认后才受理持久任务。
 * 参数说明：
 * - scope：明确选择的来源及坐标。
 * 返回：可解释影响预览；保护未解除时关闭提交按钮。
 */
async function physicalPreview(scope) {
  const preview = await api("deletion-previews", "POST", scope);
  const panel = element("div", undefined, "panel");
  panel.append(element("h2", "物理清除影响"));
  panel.append(
    element(
      "p",
      `${scope.repository} / ${scope.name} @ ${scope.version || "全部版本"} · 可回收 ${preview.reclaimable_bytes.toLocaleString()} 字节`,
    ),
  );
  panel.append(
    element(
      "p",
      preview.refetch_allowed
        ? "公开缓存清理不封禁名称或版本，下次仍允许按同一来源和摘要重新获取。"
        : "选定历史修订的访问撤销会持久保留，同摘要以后重新上传也不会自动恢复旧修订。",
    ),
  );
  table(
    panel,
    [
      ["对象 SHA-256", "digest"],
      ["存储代次", "generation"],
      ["字节", "size"],
      ["共享引用", "shared"],
    ],
    preview.objects,
  );
  if (preview.blockers.length) {
    const list = element("ul");
    for (const reason of preview.blockers) {
      list.append(element("li", reason));
    }
    panel.append(element("h2", "当前保护阻挡"), list);
  }
  const submit = actionButton(
    "确认受理此清除范围",
    async () => {
      await api(
        `deletion-previews/${encodeURIComponent(preview.id)}/commit`,
        "POST",
        { fingerprint: preview.fingerprint },
      );
      await show("deletions");
    },
    false,
  );
  submit.disabled = preview.blockers.length > 0;
  panel.append(submit);
  document.querySelector("#content").append(panel);
  panel.scrollIntoView({ behavior: "smooth", block: "start" });
}

/**
 * 业务作用：按独立公开来源展示缓存，并为有全域运维权限的用户提供不封禁的清理入口。
 * 参数说明: 无。
 * 返回：来源选择、缓存状态和精确版本清理预览按钮。
 */
async function proxyPage() {
  const proxies = (await api("repositories")).proxies;
  const content = heading(
    "公开缓存",
    "包体已保存后复用本地字节，索引刷新与包体缓存分别管理。",
  );
  if (!proxies.some((proxy) => proxy.id === selectedProxy)) {
    selectedProxy = proxies[0]?.id || "";
  }
  if (!selectedProxy) {
    content.append(element("p", "当前未启用公开代理。"));
    return;
  }
  const node = form(content, "查询来源", async (values) => {
    selectedProxy = values.proxy;
    proxyCursor = "";
    await show("proxy");
  });
  const label = element("label", "独立来源");
  const select = element("select");
  select.name = "proxy";
  for (const proxy of proxies) {
    const option = element("option", proxy.name);
    option.value = proxy.id;
    option.selected = proxy.id === selectedProxy;
    select.append(option);
  }
  label.append(select);
  node.append(label);
  finishForm(node);
  const data = await api(
    `proxies/${encodeURIComponent(selectedProxy)}/packages?after=${encodeURIComponent(proxyCursor)}`,
  );
  const manage =
    ["admin", "operator"].includes(currentUser.role) &&
    currentUser.repositories.includes("*");
  table(
    content,
    [
      ["包名", "name"],
      ["版本", "version"],
      ["SHA-256", "digest"],
      ["本地缓存", "cached"],
    ],
    data.items,
    manage
      ? (row) =>
          actionButton(
            "清理缓存预览",
            () =>
              physicalPreview({
                kind: "proxy",
                repository: selectedProxy,
                name: row.name,
                version: row.version,
              }),
            false,
          )
      : null,
  );
  if (data.next) {
    content.append(
      actionButton("下一页", async () => {
        proxyCursor = data.next;
      }),
    );
  }
}

/**
 * 业务作用：读取持久删除任务的实际进度，页面关闭不取消已经受理的工作。
 * 参数说明: 无。
 * 返回：任务状态和实际释放字节，活动任务存在时有界轮询。
 */
async function deletionPage() {
  const tasks = await api("deletions");
  const content = heading(
    "清除任务",
    "受理与完成分别记录。只有 completed 任务的 released_bytes 表示实际处理后提交的释放字节。",
  );
  const rows = tasks.map((task) => ({
    ...task,
    coordinate: `${task.scope.repository} / ${task.scope.name} @ ${task.scope.version || "全部版本"}`,
    kind: task.scope.kind,
  }));
  table(
    content,
    [
      ["任务 UUID", "id"],
      ["来源", "kind"],
      ["精确范围", "coordinate"],
      ["阶段", "state"],
      ["实际释放字节", "released_bytes"],
      ["失败次数", "attempts"],
      ["最近失败类别", "last_error_code"],
    ],
    rows,
    (row) =>
      row.last_error_code && row.state !== "completed"
        ? actionButton("立即重试", () =>
            api(`deletions/${encodeURIComponent(row.id)}/retry`, "POST"),
          )
        : null,
  );
  content.append(actionButton("刷新状态", async () => {}));
  if (tasks.some((task) => ["accepted", "running"].includes(task.state))) {
    taskRefresh = setTimeout(() => {
      if (currentUser && currentView === "deletions") {
        show("deletions");
      }
    }, 2000);
  }
}

/**
 * 业务作用：按已授权仓库分页浏览包、版本与可见性，管理动作始终走服务端影响预览。
 * 参数说明: 无。
 * 返回：包列表、历史入口及当前角色可用的管理按钮。
 */
async function packagePage() {
  const repositories = (await api("repositories")).hosted;
  if (
    !repositories.some((repository) => repository.id === selectedRepository)
  ) {
    selectedRepository = repositories[0]?.id || "";
  }
  const content = heading(
    "包与修订",
    "查看当前可见版本，保留独立仓库身份和历史修订。",
  );
  if (!selectedRepository) {
    content.append(element("p", "当前账户没有可读取的 hosted 仓库。"));
    return;
  }
  const filter = form(content, "查询", async (values) => {
    selectedRepository = values.repository;
    packageName = values.name.trim();
    packageCursor = "";
    await show("packages");
  });
  const label = element("label", "仓库");
  const selection = element("select");
  selection.name = "repository";
  for (const repository of repositories) {
    const option = element(
      "option",
      `${repository.name} · ${repository.policy}`,
    );
    option.value = repository.id;
    option.selected = repository.id === selectedRepository;
    selection.append(option);
  }
  label.append(selection);
  filter.append(label);
  input(filter, "精确包名，可留空", "name", "text", packageName).required =
    false;
  finishForm(filter);
  const query = new URLSearchParams({ after: packageCursor });
  if (packageName) {
    query.set("name", packageName);
  }
  const data = await api(
    `repositories/${encodeURIComponent(selectedRepository)}/packages?${query}`,
  );
  const rows = data.items.map((item) => ({
    ...item,
    state: item.current
      ? item.current.entry.yanked
        ? "已 yank"
        : "当前可见"
      : "已移除",
    digest: item.current?.entry.cksum || "—",
  }));
  const manage = ["admin", "operator"].includes(currentUser.role);
  table(
    content,
    [
      ["包名", "name"],
      ["版本", "version"],
      ["可见性", "state"],
      ["SHA-256", "digest"],
    ],
    rows,
    (row) => {
      const actions = element("div");
      actions.append(actionButton("历史", () => historyPage(row), false));
      if (manage && row.current) {
        for (const [operation, title] of [
          [
            row.current.entry.yanked ? "unyank" : "yank",
            row.current.entry.yanked ? "Unyank" : "Yank",
          ],
          ["remove", "移除版本"],
        ]) {
          actions.append(
            actionButton(
              title,
              () =>
                previewAction({
                  repository: selectedRepository,
                  name: row.name,
                  version: row.version,
                  operation,
                }),
              false,
            ),
          );
        }
        actions.append(
          actionButton(
            "移除该包全部版本",
            () =>
              previewAction({
                repository: selectedRepository,
                name: row.name,
                all_versions: true,
                operation: "remove",
              }),
            false,
          ),
        );
      }
      return actions;
    },
  );
  if (data.next) {
    content.append(
      actionButton("下一页", async () => {
        packageCursor = data.next;
      }),
    );
  }
}

/**
 * 业务作用：展示并维护明确的快照保留权，解除保留不直接删除数据。
 * 参数说明: 无。
 * 返回：有权限仓库的保留点与建立表单，全部写操作仍由服务端复验。
 */
async function retentionPage() {
  const repositories = (await api("repositories")).hosted;
  if (
    !repositories.some((repository) => repository.id === selectedPinRepository)
  ) {
    selectedPinRepository = repositories[0]?.id || "";
  }
  const content = heading(
    "快照保留",
    "显式保留仍可读的历史快照。HEAD、活动租约及其他保留点独立保护内容。",
  );
  if (!selectedPinRepository) {
    content.append(element("p", "当前没有有权管理的 hosted 仓库。"));
    return;
  }
  const selection = element("select");
  selection.setAttribute("aria-label", "保留点仓库");
  for (const repository of repositories) {
    const option = element("option", repository.name);
    option.value = repository.id;
    option.selected = repository.id === selectedPinRepository;
    selection.append(option);
  }
  selection.addEventListener("change", async () => {
    selectedPinRepository = selection.value;
    await show("retention");
  });
  content.append(selection);
  const path = `repositories/${encodeURIComponent(selectedPinRepository)}/pins`;
  const pins = await api(path);
  table(
    content,
    [
      ["保留点 UUID", "id"],
      ["快照 UUID", "snapshot_id"],
      ["保留主体", "actor"],
      ["原因", "reason"],
    ],
    pins,
    (pin) =>
      actionButton("解除此保留点", () =>
        api(`${path}/${encodeURIComponent(pin.id)}/release`, "POST"),
      ),
  );
  const current = repositories.find(
    (repository) => repository.id === selectedPinRepository,
  );
  const panel = element("div", undefined, "panel");
  panel.append(element("h2", "建立保留点"));
  const node = form(panel, "保留此快照", async (values) => {
    await api(path, "POST", values);
    await show("retention");
  });
  input(node, "同仓快照 UUID", "snapshot", "text", current.head);
  input(node, "保留原因，不填写凭据", "reason");
  finishForm(node);
  content.append(panel);
}

/**
 * 业务作用：为用户创建和撤销独立 Cargo 凭据，令牌正文只展示一次。
 * 参数说明: 无。
 * 返回：令牌列表与当前用户可授予的创建表单。
 */
async function tokenPage() {
  const data = await api("tokens");
  const content = heading(
    "访问令牌",
    "为开发机和 CI 分别创建凭据，业务项目通过 NAXUS_TOKEN 或受保护文件读取。",
  );
  table(
    content,
    [
      ["用途", "label"],
      ["权限", "permission"],
      ["允许覆盖已有坐标", "allow_overwrite"],
      ["发布包名限制", "publish_crates"],
      ["仓库 UUID", "repositories"],
      ["到期时间", "expires_at"],
      ["已撤销", "revoked"],
    ],
    data.map((token) => ({
      ...token,
      expires_at: localTime(token.expires_at),
    })),
    (row) =>
      row.revoked
        ? null
        : actionButton("撤销", () =>
            api(`tokens/${encodeURIComponent(row.id)}/revoke`, "POST"),
          ),
  );
  const panel = element("div", undefined, "panel");
  panel.append(element("h2", "创建访问令牌"));
  const node = form(panel, "创建令牌", async (values) => {
    const result = await api("tokens", "POST", {
      label: values.label,
      permission: values.permission,
      allow_overwrite: values.allow_overwrite === "on",
      publish_crates: values.publish_crates
        .split(",")
        .map((name) => name.trim())
        .filter(Boolean),
      repositories: values.repositories
        .split(",")
        .map((value) => value.trim())
        .filter(Boolean),
      ttl_seconds: Number(values.days) * 86400,
    });
    await show("tokens");
    const secret = element("div", undefined, "secret");
    secret.append(
      element("h2", "请现在保存令牌"),
      element(
        "p",
        "离开本页面后无法再次读取此令牌。请存入受保护文件或 CI secret。",
      ),
    );
    const text = element("textarea");
    text.readOnly = true;
    text.value = result.token;
    text.setAttribute("aria-label", "一次性访问令牌");
    secret.append(text);
    document.querySelector("#content").append(secret);
  });
  input(node, "用途标签", "label");
  const permission = input(
    node,
    "权限：read 或 publish",
    "permission",
    "text",
    "read",
  );
  permission.pattern = "read|publish";
  input(
    node,
    "仓库 UUID，逗号分隔；全域授权使用 *",
    "repositories",
    "text",
    currentUser.repositories.join(","),
  );
  const overwrite = input(
    node,
    "明确允许覆盖已有坐标及修改 yank 状态（仅 publish）",
    "allow_overwrite",
    "checkbox",
  );
  overwrite.required = false;
  overwrite.value = "on";
  const crateScope = input(
    node,
    "可发布的精确包名，逗号分隔；留空使用仓库范围",
    "publish_crates",
  );
  crateScope.required = false;
  const days = input(node, "有效天数，1 至 365", "days", "number", "30");
  days.min = "1";
  days.max = "365";
  finishForm(node);
  content.append(panel);
}

/**
 * 业务作用：管理员管理账户启用状态并明确新用户的角色和仓库范围。
 * 参数说明: 无。
 * 返回：用户列表及创建表单，口令不进入查询结果。
 */
async function userPage() {
  const data = await api("users");
  const content = heading(
    "用户管理",
    "用户角色限定操作能力，仓库 UUID 限定可访问的业务范围。",
  );
  table(
    content,
    [
      ["用户名", "username"],
      ["角色", "role"],
      ["仓库 UUID", "repositories"],
      ["已启用", "enabled"],
    ],
    data,
    (row) => {
      const actions = element("div");
      actions.append(
        actionButton(row.enabled ? "禁用" : "启用", () =>
          api(`users/${encodeURIComponent(row.id)}/enabled`, "POST", {
            enabled: !row.enabled,
          }),
        ),
      );
      actions.append(
        actionButton(
          "修改授权",
          async () => {
            editingUser = row;
            await show("users");
          },
          false,
        ),
      );
      return actions;
    },
  );
  if (editingUser) {
    const target = editingUser;
    const editor = element("div", undefined, "panel");
    editor.append(
      element("h2", `修改 ${target.username} 的授权`),
      element(
        "p",
        "提交后该用户已有会话和 token 全部失效，需要重新登录并按新范围创建凭据。",
      ),
    );
    const edit = form(editor, "确认更新授权", async (values) => {
      await api(`users/${encodeURIComponent(target.id)}/grants`, "POST", {
        role: values.role,
        repositories: values.repositories
          .split(",")
          .map((value) => value.trim())
          .filter(Boolean),
      });
      editingUser = null;
      await start();
      if (currentUser) {
        await show("users");
      }
    });
    input(
      edit,
      "角色：admin / operator / developer / reader",
      "role",
      "text",
      target.role,
    ).pattern = "admin|operator|developer|reader";
    input(
      edit,
      "仓库 UUID，逗号分隔",
      "repositories",
      "text",
      target.repositories.join(","),
    ).required = false;
    finishForm(edit);
    editor.append(
      actionButton(
        "取消修改",
        async () => {
          editingUser = null;
        },
        true,
      ),
    );
    content.append(editor);
  }
  const panel = element("div", undefined, "panel");
  panel.append(element("h2", "创建用户"));
  const node = form(panel, "创建用户", async (values) => {
    await api("users", "POST", {
      username: values.username,
      password: values.password,
      role: values.role,
      repositories: values.repositories
        .split(",")
        .map((value) => value.trim())
        .filter(Boolean),
    });
    node.reset();
    await show("users");
  });
  input(node, "用户名", "username");
  input(node, "初始口令", "password", "password");
  const role = input(
    node,
    "角色：admin / operator / developer / reader",
    "role",
    "text",
    "reader",
  );
  role.pattern = "admin|operator|developer|reader";
  input(node, "仓库 UUID，逗号分隔", "repositories", "text", "*");
  finishForm(node);
  content.append(panel);
}

/**
 * 业务作用：读取安全操作的有限审计窗口，不显示认证材料。
 * 参数说明: 无。
 * 返回：服务端已经提交的操作记录。
 */
async function auditPage() {
  const data = await api(`audit?before=${auditBefore}`);
  const content = heading(
    "安全审计",
    "查看账户、会话与访问令牌的持久操作记录。",
  );
  table(
    content,
    [
      ["序号", "sequence"],
      ["操作", "action"],
      ["主体 UUID", "actor"],
      ["对象", "target"],
      ["时间", "created_at"],
    ],
    data.map((record) => ({
      ...record,
      created_at: localTime(record.created_at),
    })),
  );
  if (auditBefore !== 0) {
    content.append(
      actionButton("查看最新", async () => {
        auditBefore = 0;
      }),
    );
  }
  if (data.length === 200) {
    content.append(
      actionButton("更早记录", async () => {
        auditBefore = data[data.length - 1].sequence;
      }),
    );
  }
}

/**
 * 业务作用：切换到已授权的管理页面，服务端仍在每个接口执行权限裁决。
 * 参数说明：
 * - view：固定页面标识。
 * 返回：当前页面显示完成，失败更新安全通知。
 */
async function show(view) {
  // 旧按钮和已排队轮询不能在身份关闭后重新开放业务页面。
  if (!currentUser) {
    return;
  }
  if (taskRefresh !== null) {
    clearTimeout(taskRefresh);
    taskRefresh = null;
  }
  currentView = view;
  notify(null);
  for (const button of document.querySelectorAll("nav button")) {
    button.classList.toggle("active", button.dataset.view === view);
  }
  try {
    if (view === "password") {
      passwordPage();
    } else if (view === "tokens") {
      await tokenPage();
    } else if (view === "users") {
      await userPage();
    } else if (view === "audit") {
      await auditPage();
    } else if (view === "packages") {
      await packagePage();
    } else if (view === "proxy") {
      await proxyPage();
    } else if (view === "deletions") {
      await deletionPage();
    } else if (view === "retention") {
      await retentionPage();
    } else {
      await repositoryPage();
    }
  } catch (error) {
    notify(error);
  }
}

/**
 * 业务作用：从服务端恢复当前身份并建立相应导航，初始换密状态不显示业务管理入口。
 * 参数说明: 无。
 * 返回：登录、换密、已授权管理界面，或依赖不可用时的重新连接入口。
 */
async function start() {
  try {
    const identity = await api("session");
    if (
      !identity ||
      typeof identity.username !== "string" ||
      !identity.username ||
      !["admin", "operator", "developer", "reader"].includes(identity.role) ||
      typeof identity.must_change !== "boolean" ||
      !Array.isArray(identity.repositories) ||
      !identity.repositories.every((scope) => typeof scope === "string")
    ) {
      throw new AdminApiError(
        200,
        "invalid_response",
        "仓库服务返回了无效会话信息。",
        null,
      );
    }
    currentUser = identity;
  } catch (error) {
    if (error?.code === "request_cancelled") {
      return;
    }
    if (!(error instanceof AdminApiError && error.status === 401)) {
      clearIdentity();
      const content = heading(
        "仓库服务暂不可用",
        "暂时无法确认登录状态，请稍后重新连接。",
      );
      content.append(actionButton("重新连接", start, false));
    }
    notify(error);
    return;
  }
  const account = document.querySelector("#account");
  account.replaceChildren(
    element("span", `${currentUser.username} · ${currentUser.role}`),
  );
  account.append(
    actionButton(
      "退出",
      async () => {
        await api("logout", "POST");
        currentUser = null;
        loginPage();
      },
      false,
    ),
  );
  const navigation = document.querySelector("#navigation");
  navigation.replaceChildren();
  const entries = currentUser.must_change
    ? []
    : [
        ["repositories", "仓库总览"],
        ["packages", "包与修订"],
        ["proxy", "公开缓存"],
        ["tokens", "访问令牌"],
      ];
  if (!currentUser.must_change && currentUser.role === "admin") {
    entries.push(["users", "用户管理"]);
  }
  if (
    !currentUser.must_change &&
    ["admin", "operator"].includes(currentUser.role)
  ) {
    entries.push(
      ["retention", "快照保留"],
      ["deletions", "清除任务"],
      ["audit", "安全审计"],
    );
  }
  entries.push(["password", "更换口令"]);
  for (const [view, label] of entries) {
    const button = element("button", label);
    button.dataset.view = view;
    button.addEventListener("click", () => show(view));
    navigation.append(button);
  }
  await show(currentUser.must_change ? "password" : "repositories");
}

start();
