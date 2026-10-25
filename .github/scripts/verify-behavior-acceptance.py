import json
import os
import re
import subprocess

REQUIRED_SUITES = {
    "publication_recovery",
    "resolution_binding",
    "authorization",
    "deletion_recovery",
    "proxy_cache",
    "backup_restore",
    "linux_clients",
}


def require(condition, message):
    """业务作用：执行不能被 Python 优化模式跳过的发布拒绝门禁。
    参数说明：
    - condition：必须已经成立的身份、终态或覆盖条件。
    - message：不含敏感材料的拒绝原因。
    返回：条件成立时继续，证据不足时抛出异常并停止发布。
    """
    if not condition:
        raise ValueError(message)


def verify(check, revision, application_id, images):
    """业务作用：将行为验收绑定到受信任发布者、精确源码和待上传镜像，静态构建不能替代该凭据。
    参数说明：
    - check：由 GitHub API 回读的独立 Check Run。
    - revision：本次发布的精确提交 SHA。
    - application_id：运维明确配置的行为验收 GitHub App ID。
    - images：两种架构实际待上传的镜像 config 摘要。
    返回：来源、终态和全部行为范围一致才成功；缺失配置或不匹配会在登录 registry 前阻止发布。
    """
    require(
        application_id.isdecimal() and int(application_id) > 0,
        "Trusted behavior App ID is required",
    )
    # 同名检查可由其他集成创建，只有明确受信任的 App 才有权证明行为验收。
    require(
        check.get("app", {}).get("id") == int(application_id),
        "Unexpected behavior acceptance publisher",
    )
    require(
        check.get("name") == "naxus-behavior-acceptance",
        "Unexpected acceptance contract",
    )
    require(
        check.get("head_sha") == revision, "Behavior acceptance targets another source"
    )
    require(
        check.get("status") == "completed" and check.get("conclusion") == "success",
        "Behavior acceptance is not successful",
    )
    require(
        str(check.get("details_url", "")).startswith("https://"),
        "Traceable acceptance evidence is required",
    )
    text = check.get("output", {}).get("text", "")
    require(
        isinstance(text, str) and len(text) <= 1024 * 1024,
        "Invalid acceptance evidence",
    )
    evidence = json.loads(text)
    require(
        evidence.get("contract") == "naxus.behavior-acceptance",
        "Unsupported evidence contract",
    )
    # 编译提交与二进制必须同时匹配，旧凭据不能为后来重建的镜像开放上传。
    require(
        evidence.get("head_sha") == revision and evidence.get("images") == images,
        "Validated artifacts differ from publication",
    )
    suites = evidence.get("suites", {})
    require(
        isinstance(suites, dict)
        and all((suites.get(name) is True for name in REQUIRED_SUITES)),
        "Required behavior coverage is missing",
    )


def main():
    """业务作用：在 Docker Hub 上传前回读独立行为凭据，拒绝仅凭编译成功进行公开发布。
    参数说明: 无。
    返回：验收与当前提交及两架构镜像完全绑定时退出零，无法核实时停止发布。
    """
    check_id = os.environ["BEHAVIOR_CHECK_RUN_ID"]
    require(re.fullmatch("[0-9]+", check_id), "Invalid behavior check run ID")
    images = {
        architecture: os.environ[f"{architecture.upper()}_CONFIG_DIGEST"]
        for architecture in ("amd64", "arm64")
    }
    require(
        all((re.fullmatch("sha256:[a-f0-9]{64}", value) for value in images.values())),
        "Invalid accepted image digest",
    )
    repository = os.environ["GITHUB_REPOSITORY"]
    raw = subprocess.check_output(
        ["gh", "api", f"repos/{repository}/check-runs/{check_id}"]
    )
    require(len(raw) <= 2 * 1024 * 1024, "Behavior check response exceeds limit")
    verify(
        json.loads(raw),
        os.environ["GITHUB_SHA"],
        os.environ.get("BEHAVIOR_CHECK_APP_ID", ""),
        images,
    )
    print("Independent behavior acceptance matches the source and image artifacts.")


if __name__ == "__main__":
    main()
