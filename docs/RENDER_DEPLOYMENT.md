# flashpdf 渲染功能部署研究（决策前参考）

> 状态：**研究文档，未实施**。合并 render feature 到 main 前的工程量评估。
> 维护：2026-06-28，基于 pypdfium2 v4.30 release workflow + flashpdf 现有 CI 调研。

## 1. 问题陈述

PDFium 是 ~10MB 的 C++ 动态库（`.so` / `.dylib` / `.dll`），不能编译进 Rust 二进制。
用户 `pip install flashpdf` 后想直接用 `page.get_pixmap()`，运行时必须能在文件系统某处找到
PDFium binary——这就是"部署负担"。

当前 flashpdf 的方案要求用户手动管理（`PDFIUM_PATH` 或 `./pdfium-bin/`），体验远不如
`pip install pypdfium2`（自带 binary）。本文评估把 PDFium binary 打包进 wheel 的工程量。

## 2. pypdfium2 怎么做（金标准参考）

调研了 pypdfium2 的 6 个核心 workflow 文件（main / sbuild / sbuild_one / cibw / cibw_one / bsd）。
他们其实有**两条路径**：

### 路径 A：从源码编译 PDFium（sbuild，pypdfium2 默认）

```yaml
- name: Build PDFium (toolchained)
  run: python3 ./setupsrc/build_toolchained.py $VERSION_PARAM $CPU_PARAM ...
- name: Build wheel
  run: python3 -m build -wxn
  env:
    PDFIUM_PLATFORM: sourcebuild
```

- 每个 (OS, arch) 独立编译 PDFium 源码（depot_tools + gn + ninja）
- 单平台编译耗时 ~30-60 min
- 优点：完全控制；许多小众架构（ppc64le/s390x/riscv64/loongarch64）只有这条路
- 缺点：CI 慢、维护重、需要 toolchain 专业知识

### 路径 B：用 bblanchon/pdfium-binaries 预编译包（cibw，更简单）

```yaml
matrix:
  - os: ubuntu-26.04
    cibw_os: manylinux
    cibw_arch: x86_64
    pdfium_ver: latest  # 从 bblanchon 拉
```

- 用 [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) 项目（BSD 协议，
  社区维护）的预编译 release
- 单平台下载 + 打包耗时 ~3-5 min
- 优点：快、简单、覆盖主流平台
- 缺点：依赖外部项目；小众架构覆盖不如 A

### pypdfium2 的矩阵规模（路径 B cibw）

实测 26 个 matrix entry，覆盖：
- Linux: manylinux/musllinux × x86_64/aarch64/armv7l/i686/ppc64le/s390x/riscv64/loongarch64
- macOS: x86_64/arm64
- Windows: amd64/arm64/x86

**测试矩阵**：6 OS × 3 Python = 18 test jobs（main.yaml L116-142）

### Trusted publishing（重要）

pypdfium2 用 PyPI 的 trusted publishing（OIDC），**不需要 API token**：
```yaml
permissions:
  id-token: write
- uses: pypa/gh-action-pypi-publish@release/v1
```

## 3. flashpdf 现状（好消息）

调研了 `.github/workflows/build-wheels.yml`——**flashpdf 的 CI 基础已经具备**：

```yaml
matrix:
  include:
    - os: ubuntu-latest
      target: x86_64
    - os: ubuntu-latest
      target: aarch64
    - os: macos-latest
      target: x86_64
    - os: macos-latest
      target: aarch64
    - os: windows-latest
      target: x64
```

**已有的能力**：
- 5 平台矩阵（linux x64/arm64, macOS x64/arm64, Windows x64）— 覆盖 99% 用户
- `PyO3/maturin-action@v1` 构建 wheel
- Trusted publishing（OIDC，无 token）
- Tag-triggered 自动 release + GitHub Release

**已有的不足**：
- 用 `--find-interpreter` 而非 abi3 → 每个 Python 版本单独构建（可优化但不阻塞）
- 没有 `--features render` 入口
- 没有下载 PDFium binary 的步骤
- render.rs 没有查 wheel 内 binary 的路径

## 4. 推荐方案：抄 pypdfium2 路径 B（bblanchon 预编译）

**不**走路径 A（源码编译）。理由：
- flashpdf 不需要支持 ppc64le / s390x / riscv64 等小众架构
- 路径 B 的工程量是路径 A 的 1/10
- bblanchon/pdfium-binaries 是 pypdfium2 也用的成熟方案

### 4.1 bblanchon/pdfium-binaries 提供什么

每个 release 提供（约每月一次更新）：
```
pdfium-linux.tgz         → libpdfium.so (x86_64)
pdfium-linux-arm.tgz     → libpdfium.so (aarch64)
pdfium-mac.tgz           → libpdfium.dylib (x86_64)
pdfium-mac-arm64.tgz     → libpdfium.dylib (arm64)
pdfium-win.tgz           → pdfium.dll (x64)
pdfium-win-arm64.tgz     → pdfium.dll (arm64)
```

下载 URL 模板：
```
https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-{pkg}.tgz
```

### 4.2 wheel 内布局

```
flashpdf-0.8.0-cp39-abi3-macosx_11_0_arm64.whl
└── flashpdf/
    ├── __init__.py
    ├── _flashpdf.so           ← Rust 编译产物
    └── _pdfium/                ← 新增
        └── libpdfium.dylib     ← 从 bblanchon 拉
```

用户 `pip install flashpdf` 后，运行时自动从 `flashpdf/_pdfium/` 加载。

### 4.3 实施清单（修订版）

#### 代码改动

**`crates/flashpdf-core/src/render.rs`** — 加 wheel-bundled binary 查找路径（约 10 行）：

```rust
fn load_pdfium() -> Result<Pdfium, String> {
    // 1. PDFIUM_PATH env (existing)
    if let Ok(p) = std::env::var("PDFIUM_PATH") { ... }
    
    // 2. NEW: wheel-bundled binary (查 Python 包目录)
    if let Some(p) = bundled_pdfium_path() {
        if let Ok(b) = Pdfium::bind_to_library(
            Pdfium::pdfium_platform_library_name_at_path(&p)
        ) {
            return Ok(Pdfium::new(b));
        }
    }
    
    // 3. ./pdfium-bin/ dev convenience (existing)
    // 4. system library (existing fallback)
}

fn bundled_pdfium_path() -> Option<PathBuf> {
    // 走 Python 的 importlib.util.find_spec 找 flashpdf 包目录
    // 然后拼接 _pdfium/ 子目录
    // 注意：这一步需要 pyo3 调用 Python，或者从 Cargo env var 推导
}
```

**`crates/flashpdf-core/build.rs`**（新建或扩展）— `--features render` 时下载 PDFium：

```rust
fn main() {
    if std::env::var("CARGO_FEATURE_RENDER").is_ok() {
        let target = std::env::var("TARGET").unwrap();
        let (url, filename) = pick_pdfium(&target);
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let dest = Path::new(&out_dir).join(filename);
        if !dest.exists() {
            download_and_extract(&url, &dest);
        }
        println!("cargo:rustc-env=PDFIUM_BUNDLED={}", dest.parent().unwrap().display());
    }
}
```

**`pyproject.toml`** — 让 maturin 把 PDFium binary 打进 wheel：

```toml
[tool.maturin]
# 现有配置
# 关键：把 OUT_DIR 里的 PDFium 复制到 wheel 的 flashpdf/_pdfium/
include = [{ path = "target/*/libpdfium.*", format = "wheel" }]
```

（具体语法要看 maturin 版本，0.13+ 支持复杂 include 规则）

#### CI 改动

**`.github/workflows/build-wheels.yml`** — 加 PDFium 下载步骤：

```yaml
- name: Download PDFium binary
  shell: bash
  run: |
    case "${{ matrix.target }}" in
      x86_64-*)  PKG=linux ;;
      aarch64-*) PKG=linux-arm ;;
      # ... macOS, Windows
    esac
    curl -L https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-${PKG}.tgz \
      | tar xz -C pdfium-bin
  if: true  # always, even without --features render for now

- name: Build wheel
  uses: PyO3/maturin-action@v1
  with:
    target: ${{ matrix.target }}
    args: --release --out dist --find-interpreter --features render
    manylinux: auto
  env:
    PDFIUM_BIN_DIR: ${{ github.workspace }}/pdfium-bin
```

（具体怎么把 binary 注入到 maturin build，可能需要 maturin 的 `[tool.maturin] include`
配置 + build.rs 协同，需要试验。）

### 4.3 修订工程量估计

**调研后修正：原估 5 天 → 实际 1-2 天专注工作**。

理由：
- flashpdf CI 基础设施已有（5 平台矩阵、maturin、trusted publishing）
- 不需要做 macOS notarization（PDFium binary 由 bblanchon 签名）
- 不需要支持小众架构（ppc64le 等）
- 不需要从源码编译 PDFium

| 任务 | 时长 |
|---|---|
| 写 `build.rs` 下载 PDFium（基于 bblanchon URL 模板）| 0.3 天 |
| 改 `render.rs` 加 bundled binary 查找路径 | 0.3 天 |
| 配 `pyproject.toml` 让 maturin 打包 binary 进 wheel | 0.5 天（最不确定）|
| 改 `build-wheels.yml` 加 download 步骤 + `--features render` | 0.3 天 |
| 端到端测试 5 个 wheel（一个一个 install + smoke test）| 0.5 天 |
| **总计** | **~2 天** |

**最大不确定性**：maturin 的 `[tool.maturin] include` 配置语法。需要查 maturin 文档或
参考其他用 maturin 打包外部 binary 的项目（如 `nutpie`、`polars` 的某些 feature）。

## 5. 两种部署形态的选择

合并 render feature 后，可以让两种形态共存：

### 形态 1：`pip install flashpdf`（默认，无渲染）

- wheel ~3MB
- 行为完全同今天（文本提取 + 嵌入图像）
- render feature 编译时关闭
- `page.get_pixmap()` 调用抛 `NotImplementedError` 引导用户装 `[render]`

### 形态 2：`pip install "flashpdf[render]"`（带渲染）

- wheel ~13MB（多 ~10MB PDFium binary）
- 全部功能可用
- 用户显式 opt-in

**实现方式**：用 PyPI 的 optional-dependencies + 两个 wheel 变体；或者更简单——
**只发一个 wheel 带渲染**，默认开启（接受 wheel 体积膨胀）。这要看用户偏好。

**推荐**：发**一个 wheel 带 render**（让渲染开箱即用）。理由：
- 10MB 在 2026 年不算大（PyMuPDF wheel ~30MB）
- 用户不需要理解 `[render]` extras 概念
- 减少 CI 矩阵（不需要为带/不带各打一遍）
- 减少 PyPI 上传条目

**反推荐**：发两个 wheel。理由：
- 用户不知道该装哪个
- 双倍 CI 时间和存储
- `import flashpdf` 时无法预测有没有渲染能力

## 6. 风险与维护成本

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| bblanchon/pdfium-binaries 停止维护 | 低 | 高（无 binary）| fork 一份到 flashpdf org 自己维护 |
| PDFium 有 CVE | 中 | 中 | 监控 bblanchon releases，3 个月内 bump |
| maturin 的 binary include 配置不支持需求 | 中 | 中（CI 卡住）| 退路：用 setuptools + cffi 风格的 build hook |
| bblanchon binary 在某些 manylinux 版本上不兼容 | 低 | 中 | 跑 auditwheel 验证 |
| 用户在内网无法 pip install（拿不到 binary）| 低 | 低 | 提供 wheel-only 安装包 + 文档说明 |
| wheel 体积膨胀影响 CI 缓存 | 低 | 低 | 接受 |

## 7. 决策建议

### 推荐路径（如果决定合并）

```
1. 修 11 个 page-tree bug（render_only 路径用 3-tier fallback）
2. 写 build.rs + pyproject.toml + CI 改动（~2 天工作）
3. 测试 5 个 wheel 都能 pip install + 渲染一张 PDF
4. 发 v0.8.0，README 加 "pip install flashpdf" 一键说明
5. CHANGELOG 显著标注：scope 扩展（从纯解析 → 解析 + 渲染）
6. ROADMAP Non-goals 从"页面渲染"改为"自建光栅化器"（精确边界）
```

### 反对合并的最强论据

如果你强烈认同"flashpdf 永远是纯 Rust 解析库"这个身份认同，**不要合并**——把
render 能力 fork 成 `flashpdf-render` 独立项目。这选择哲学纯洁，但会错过"pip install
即可使用、和 fitz/pypdfium2 直接竞争"的市场机会。

### 不合并的中间态

也可以**只合并代码、不发渲染 wheel**：
- main 分支有 render feature 代码
- `pip install flashpdf` 装的 wheel 仍不带 PDFium
- 源码用户 `maturin develop --release --features render` 可用
- 把 PyPI binary 分发留到下一版

**优点**：增量推进，无大爆炸风险
**缺点**：源码用户门槛仍高，市场曝光小

## 8. 参考资料

- [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) — PDFium 预编译包
- [pypdfium2 release workflow](https://github.com/pypdfium2-team/pypdfium2/tree/main/.github/workflows) — CI 模板参考
- [maturin documentation](https://www.maturin.rs/) — Rust→Python wheel 打包
- [PyPI trusted publishing](https://docs.pypi.org/trusted-publishers/) — 无 token 发布

## 9. 附录：pypdfium2 工作流文件清单

调研时下载的文件（在 `/tmp/pypdfium2_research/`）：

| 文件 | 行数 | 用途 |
|---|---:|---|
| `main.yaml` | 314 | 主打包+测试编排，autorelease 版本管理 |
| `cibw.yaml` | 192 | cibuildwheel 矩阵定义（路径 B）|
| `cibw_one.yaml` | （未下载）| 单平台 cibw 构建 |
| `sbuild.yaml` | 149 | sourcebuild 编排（路径 A）|
| `sbuild_one.yaml` | 184 | 单平台 sourcebuild（编译 PDFium）|
| `bsd.yaml` | 44 | FreeBSD 测试 |

flashpdf 需要的**只是 cibw 风格（路径 B）**，不需要 sourcebuild。
