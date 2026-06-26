# flashpdf Roadmap

> 起点版本：**v0.3.2**（165-PDF 病理语料 0% 失败率，平均速度 ~2× 同侪）
> 维护：随版本演进持续刷新；完成的项移到 [CHANGELOG.md](../CHANGELOG.md)。
> 本文档取代旧 `TODO.md`。

## 总体目标

让 flashpdf 从"**最快的纯文本/图像提取器**"演进为"**功能可替代 PyMuPDF 的 fastest-in-class
库**"。决策原则：

- **性能不退步**：任何新功能不能让 corpus 平均速度或失败率倒退。每个 PR 跑一遍 `bench_corpus.py`。
- **fitz 兼容优先**：新 API 尽量对齐 PyMuPDF 形态，降低用户迁移成本。
- **可观测性贯穿**：每个功能都暴露足够的诊断信息（`diagnostics` 字段、错误 context、可选 logging）。
- **零长尾失败**：扩语料测到的问题优先于性能微调。
- **无破坏性变更**：扩展字段而非修改；老用户的 `extract()` / `open()` 调用永不被打断。

## 四个阶段

| 阶段 | 主题 | 目标版本 | 核心交付 |
|------|------|---------|---------|
| 1 | fitz 功能补全 | v0.4.0 | `metadata`、链接 API、`span.flags`、TOC、CLI |
| 2 | 适用面扩大 | v0.5.0 | 加密 PDF、Linearized、错误信息、examples、迁移指南 |
| 3 | 精度深挖 | v0.6.0 | Type3、竖排文本、char_sim 残差 |
| 4 | 规模化验证 | v0.7.0 | 扩语料、tiny 性能、logging、profile |

---

## Phase 1 — fitz 功能补全（v0.4.0）

**主题**：让"从 PyMuPDF 切到 flashpdf"成为无痛迁移。

**为什么先做**：每个功能都是 fitz 用户立即注意到的缺失。技术风险低、用户感知强、
互不阻塞——可以一周一个。

**任务排序原则**：先难后易 + 快速胜利优先。先把高价值/低复杂度的（metadata、links）
打出来稳定军心，再做 flags（中复杂度、有 char_sim 回归风险），最后做 TOC（最复杂，
需要 dest→page 解析）和 CLI（包装层，永远能做完）。

### 1.1 `doc.metadata` 提取

**Scope**：fitz `doc.metadata` 返回 `{"title", "author", "subject", "keywords",
"creator", "producer", "creationDate", "modDate", "format", "encryption", "size"}` 等。

- 解析 `/Info` 字典（PDF spec §14.3.3），全部字符串走 escape + UTF-16 BOM 解码
- 暴露到 `PyDocument.metadata` getter（返回 dict）
- 缺字段时返回 `None`，与 fitz 一致

**Verification**：
- 单元测试：构造 `/Info` 含 6 个字段的 PDF，断言全部正确解出
- 对照：vs fitz 在 5 个真实 PDF（学术论文、Office 导出、扫描件）上字段级一致

**风险**：低（纯解析）

**复杂度**：低

### 1.2 链接提取 Python API

**Scope**：CHANGELOG 提到 Rust 侧 `extract_links` 已实现，但没接 pyo3。

- 暴露 `page.get_links()` → `[{"kind": "uri"|"goto"|"named", ...}]`
- 对齐 fitz `Link` 字段：`from` bbox, `kind`, `to` page, `uri`, ...
- 加到 `PyPage`

**Verification**：
- 对照 vs fitz 在带超链接的 arxiv PDF 上的输出
- 加 1 个单元测试覆盖 uri 链接 + 内部 goto

**风险**：低（核心逻辑已实现）

**复杂度**：低

### 1.3 `span.flags` 格式探测

**Scope**：fitz 用 bitmask 编码 italic(2^1) / bold(2^4) / serif(2^2) / monospaced(2^3) /
superscript(2^0) 等。当前 `flags=0` stub。

- 解析 `/FontDescriptor >> /Flags`（按 PDF spec §7.9.2：
  bit 1=FixedPitch, bit 4=Symbolic, bit 6=Nonsymbolic, bit 7=Italic）
- `/BaseFont` 名称启发式匹配（`*Bold*`, `*Italic*`, `*Oblique*`, `*Mono*`, `*Courier*`）
- 装到 `TextSpan.flags: u32`，在 `emit_string` 时计算一次
- pyo3 侧暴露到 span 字典

**Verification**：
- **合成 ground truth PDF**（用 reportlab 或手写 PDF）：明确构造含
  `/FontDescriptor /Flags 32`（Italic）、`/Flags 4`（Symbolic）等 6 种典型 bitmask，
  断言 `flags & bit != 0`。**不依赖** fitz 比较——fitz 自身 flags 在不同版本下定义
  略有差异，参考它会把第三方的解析差异误判成我们的 bug。
- 回归：arxiv_2604 / dbnet_plus char_sim 不退化（flags 不应改 text 内容，仅改元数据）

**风险**：Bold / Italic 探测在不同 fitz 版本下定义略有差异；先支持最稳的 4 位（italic/bold/serif/mono），
superscript 留到后续。

**复杂度**：中（解析已有，主要是 FontDescriptor 接入 + 启发式）

### 1.4 TOC / outline 提取

**Scope**：实现 `doc.get_toc()` → `[[level, title, page, dest?], ...]`，对齐 fitz。

- 解析 `/Root /Outlines` → `/First`/`/Next`/`/Title`/`/Dest`/`/A` 链表
- **包括 ObjStm 压缩的 Name Tree 形式**（PDF 1.5+ 把 outline 压进 ObjStm 的情况），
  走现有的 ObjStm 解码路径
- `page` 通过 `/Dest` 解引用到 page object → 页号
- 深度优先遍历，level 从 1 开始
- 处理损坏 outline（断链、循环引用）的回退

**Verification**：
- 单元测试：单层 outline / 多层嵌套 outline / 空 outline
- 对照：vs fitz `get_toc()` 在 5 个有目录的真实 PDF 上的输出
- corpus 不退化

**风险**：`/Dest` 命名引用 vs 显式引用两种语法都要支持；扫描件 PDF 偶有 page-ref 解析失败需 fallback。

**复杂度**：中（PDF spec §12.3 清晰，主要工作在 dest→page 解析 + Name Tree）

### 1.5 CLI 工具

**Scope**：`flashpdf` 命令行入口，降低试用门槛。

```
flashpdf extract paper.pdf                    # 输出 text
flashpdf extract paper.pdf --mode dict > out.json
flashpdf extract *.pdf --output-dir out/      # 批量
flashpdf info paper.pdf                       # 页数、is_scanned 概览
flashpdf toc paper.pdf                        # 打印目录
```

**技术路径：Python `click`**（非 Rust `clap`）。理由：

- 纯包装现有 `flashpdf.open()` / `extract()` API，0 跨语言复杂度
- pip 安装时自动注册 `flashpdf` 命令（`pyproject.toml [project.scripts]`），
  不需要 maturin 的双 binary 配置
- 字符串处理 / JSON 输出 / glob 展开在 Python 一行搞定
- 后续若发现 hot path（批量提取的 IO 调度）需要 Rust 加速，再下沉到 Rust；YAGNI

**Verification**：
- 手动跑 5 个真实 PDF 验证输出
- README 加 CLI 章节
- `flashpdf --help` 自描述

**风险**：低（包装现有 API）

**复杂度**：低

### Phase 1 出口标准

- [x] 1.1-1.5 全部完成且有单元测试（77 Rust + 11 CLI 全过）
- [x] `bench_corpus.py` 失败率仍 0%，速度无回退（v0.4.0 vs v0.3.2）
- [x] CHANGELOG v0.4.0 条目
- [x] README + ROADMAP 更新
- [ ] PyPI 发布 + GitHub Release（手动操作；code-side ready in `main`）

---

## Phase 2 — 适用面扩大（v0.5.0）

**主题**：处理 fitz 能处理但 flashpdf 直接 fatal 的场景。

### 2.1 加密 PDF 支持

**Scope**：目前 `Document::from_mmap` 见到 `/Encrypt` 直接 fatal。按风险/收益
**分三档**实现：

#### 2.1a RC4（V1-V2）—— PDF 1.5 标准加密，**Phase 2 必做**

- 用户密码 / 所有者密码
- 空 password 自动解密（大多数浏览器导出的"加密但无密码"PDF）
- 用 `ring` 或自实现（RC4 简单且无专利问题）

**Verification**：单元测试用 PyMuPDF 生成的 RC4 加密 PDF；进 corpus 跑 bench

**复杂度**：中

#### 2.1b AES-128（V4）—— PDF 1.5+，**Phase 2 必做**

- CBC 模式 + PKCS#7 padding
- 用 `aes` crate，**不自实现**

**Verification**：同上

**复杂度**：中

#### 2.1c AES-256（V5/R6）—— PDF 1.7 Ext Level 8，**Phase 2 可选/可延期**

- 涉及 SASLprep / SHA-256 多轮 / AES-256-CBC
- **决策点**：Phase 2 启动前先抽样统计真实比例（见开放问题），若 <1% PDF 用此变体则
  推迟到 Phase 3 或不做，文档明确标注"不支持"

**复杂度**：高

**风险（全 2.1）**：密码学代码要谨慎，**永远用审计过的 crate，绝不自实现**。

### 2.2 Linearized PDF 支持

**Scope**：Linearized PDF（PDF §7.6，"快速 Web 视图"格式）在文件开头有
冗余的 xref 表 + 第一页数据，专为流式渲染设计。当前 flashpdf 走 mmap + 全文档解析，
功能上能读但**不能利用 linearized 优势**——大文档（10MB+）首页提取时间和完整
文档差不多。

- 检测 linearized 标记（`/Linearized` key 在第一个对象）
- 实现"首页快速路径"：仅解析第一页所需对象
- 暴露 `doc.is_linearized: bool`
- **不重写整个解析器**——只在 `extract()` 加 fast-path 分支

**Verification**：
- 单元测试：构造 linearized PDF，验证首页提取时间 < 完整文档的 1/N
- 对照 fitz `doc.is_fast_webview` 一致

**风险**：低（fast-path 失败时回退到完整解析）

**复杂度**：中

### 2.3 错误信息增强

**Scope**：当前错误都是 `Message("expected 'obj' keyword")`，没字节偏移、没 context。

- `ParseError` 改成结构体：`{ kind, offset: usize, context: Vec<u8>, msg }`
- Display 时输出：`error at byte 12345: expected 'obj' keyword\n  context: ... "0000000107 00000 n" ...`
- 加 `error_chain` 让上层错误保留原始 cause

**Verification**：
- 故意损坏的 PDF 报错带 offset
- 不影响 corpus 失败率（只是文案改）

**风险**：低（纯重构）

**复杂度**：低

### 2.4 examples/ 目录

**Scope**：给"我要做 X 该怎么写"提供 copy-paste 起点。

- `examples/rag_index.py`：批量 PDF → JSON → embedding
- `examples/markdown_export.py`：dict → Markdown（标题/段落/list 启发式）
- `examples/ocr_bridge.py`：扫描页 → 图像字节 → Tesseract
- `examples/toc_to_yaml.py`：`get_toc()` → YAML

**Verification**：每个 example 在 1-2 个真实 PDF 上能跑

**风险**：无

**复杂度**：低

### 2.5 fitz 迁移指南

**Scope**：`docs/MIGRATION_FROM_FITZ.md`，覆盖常见差异。

- API 对照表（已有的+扩展）
- 输出格式差异（flags / ascender / 等）
- 不支持的功能（rendering / annotation）的替代方案
- 性能 / 稳定性差异的预期

**Verification**：让 1 个 fitz 重度用户试读，看能否独立迁移

**风险**：无

**复杂度**：低

### Phase 2 出口标准

- [ ] 加密 PDF 支持 2.1a + 2.1b（2.1c 视抽样结果决定）
- [ ] Linearized PDF 至少 fast-path 跑通
- [ ] 错误信息带 offset
- [ ] 4 个 examples 可跑
- [ ] 迁移指南
- [ ] corpus 失败率 ≤ 1%（加入加密文件后允许少量硬失败）
- [ ] PyPI 发布

---

## Phase 3 — 精度深挖（v0.6.0）

**主题**：把 char_sim 从 92-93% 推到更高，消除剩余边缘 case。

**为什么放第三阶段**：92% 已可读，剩余 7% 多为 Type3 / 竖排 / 复杂版式，技术深度高、用户感知弱。
等 1/2 阶段把"功能层面追平 fitz"后，再做这种"超越 fitz"的优化。

### 3.1 Type3 字体专门处理

**Scope**：`/Type3` 字体的字形由绘图算子定义，不走标准字体路径。

- 检测 `/Subtype /Type3`，触发专门 handler
- **唯一目标：仅走 `/ToUnicode` 路径输出 Unicode**——不做字形渲染，不做 bbox 推算
- 若 `/ToUnicode` 缺失：标记 `diagnostics.type3_char_count`，让用户决定是否 OCR
- **明确放弃"选项 A：渲染 Type3 字形"**——渲染是 MuPDF/PDF.js 的领域，与 flashpdf
  "纯解析、零渲染"的设计目标相悖（见 Non-goals）

**Verification**：
- 找 2-3 个 Type3 PDF（PyMuPDF corpus 有 `type3font.pdf`）
- 有 `/ToUnicode` 的 Type3 PDF 字符提取率达到 normal font 水平
- 无 `/ToUnicode` 的在 diagnostics 计数中显式可见

**风险**：无 `/ToUnicode` 的 Type3 字体（中文老文档常见）无法处理，只能 OCR——
这是根本性限制，文档明确标注，**可延期到 Phase 4 之后或永不做**。

**复杂度**：中（仅走 ToUnicode 路径）/ 高（如果要做渲染）

### 3.2 竖排文本聚类

**Scope**：当前旋转字符独立聚类成行（每个字符可能自成一行）。

- 探测文本矩阵的旋转方向（0/90/180/270）
- 90°/270° 字符按 Y 聚类成竖排 line，而非 X
- 输出到 `page.get_text("dict")` 时给竖排 line 标记方向
- 配合 `include_rotated=True` 默认输出

**Verification**：
- arxiv 侧栏水印读取流利（不是一字一顿）
- CJK 竖排 PDF（如有）整体可读
- 不影响横排 char_sim

**风险**：横排/竖排混合在同一页的边界判断。

**复杂度**：中

### 3.3 char_sim 残差分析

**Scope**：找出 char_sim 与 fitz 差 ~7% 的具体来源。

- 写脚本 diff flashpdf vs fitz 输出，分类残差（whitespace / order / missing chars / extra chars）
- 按分类逐个收敛
- **目标重新定义为"单调增长 + 残差报告"**——不预先承诺"≥97%"或"≥99%"，
  因为可能发现根本性差异（如 fitz 输出特定 control char）无法消除。
  目标是**每版本不退化 + 每类残差有解释**。

**Verification**：
- 残差分析报告 `docs/CHAR_SIM_AUDIT.md`，列出每类残差 + 已修/未修状态
- 每修一类，char_sim 单调上升
- corpus 整体 char_sim 不退化

**风险**：可能发现根本性差异，无法完全消除——这是诚实结论，不是失败。

**复杂度**：中（取决于残差类型）

### Phase 3 出口标准

- [ ] Type3 至少 `/ToUnicode` 路径走通
- [ ] 竖排文本可读流利
- [ ] `CHAR_SIM_AUDIT.md` 完成，每类残差有解释
- [ ] diagnostics 计数在 corpus 上整体下降
- [ ] PyPI 发布

---

## Phase 4 — 规模化验证（v0.7.0）

**主题**：在更大语料上验证稳定性，把长尾性能问题做掉。

### 4.1 扩大测试语料

**Scope**：当前只用 PyMuPDF 的 165-PDF bug-regression 测试集。补充：

- **veraPDF corpus**：1k+ PDF，合规性测试 fixtures
- **Mozilla pdf.js corpus**：~100 PDF，浏览器兼容性
- **SafeDocs corpus**：政府级 PDF，挑战样本
- **Ghostscript test suite**：渲染引擎兼容性

**先做 triage 步骤**（关键修订）：扩语料前先抽样 100 PDF 跑一次，把失败分类
（hard crash / wrong text / wrong layout / 字符级差异），按"影响用户数 × 修复难度"
排序。**避免一次性 PR 灌入 1500+ PDF 暴露 200 个边缘 case，无法排优先级**。

加 `tests/bench_corpus_extended.py` 跑多语料聚合。

**Verification**：3 个语料合起来 1500+ PDF，flashpdf 失败率 < 5%（接受少量硬失败，但每个有诊断）

**风险**：可能暴露大量边缘 case，**triage 步骤就是为了管理这个风险**。

**复杂度**：高（取决于暴露的问题量）

### 4.2 tiny 文件性能优化

**Scope**：<10KB 文件 pdf_oxide 反超 flashpdf（0.21ms fp p50 vs pdf_oxide 更快）。
启动开销主导，分析瓶颈：

- rayon 线程池 setup（warm pool vs cold）
- Python module import 时间
- 首次 mmap + xref parse

**Verification**：
- tiny 桶 fp p50 从 0.21ms 降到 ≤ 0.15ms
- 不影响 large 文件速度

**风险**：可能要做 lazy rayon init，侵入性中等。

**复杂度**：中

### 4.3 logging 模块

**Scope**：让用户能 `RUST_LOG=flashpdf=debug` 看解析过程，方便定位问题。

- 集成 `tracing` crate
- 关键节点加 span：xref parse / page extract / font load
- Python 侧暴露 `flashpdf.set_log_level("debug")`

**Verification**：
- `RUST_LOG=flashpdf_core::recovery=debug flashpdf open corrupt.pdf` 能看到恢复路径
- 不影响正常路径速度（tracing 默认 no-op when disabled）

**风险**：低（tracing 设计就是零开销）

**复杂度**：低

### 4.4 性能 profile 文档

**Scope**：`docs/PERFORMANCE.md`，写清楚 flashpdf 在哪里快、哪里慢、怎么调。

- 各场景（tiny / large / 多文件）的速度分解
- `page_parallel` / `file_parallel` 决策树
- 何时关闭 `include_images`
- 何时用 `extract()` vs `extract_many()` vs `open()`
- flamegraph 跑法

**Verification**：1 个新用户能照着调优自己场景

**风险**：无

**复杂度**：低

### Phase 4 出口标准

- [ ] 至少 2 个新语料集成到 bench（先 triage）
- [ ] tiny 文件性能提升 ≥ 25%
- [ ] logging 可用
- [ ] PERFORMANCE.md 完成
- [ ] PyPI 发布

---

## 横切关注点

每个版本都要做的，不单独成 phase：

### 测试

- 每个 PR 保持 `cargo test -p flashpdf-core` 39+ 测试全过
- 新功能必须有单元测试
- 修复类 PR 加回归测试

### 文档

- CHANGELOG 每个版本条目
- README 只保留当前最新数据，老数据移到 BENCHMARK.md
- ROADMAP.md（本文档）随版本演进刷新
- API.md 跟随新 API 更新

### CI

- 不破坏已有的 tag-triggered PyPI 发布
- **bench regression check 从 Phase 1 起生效**（不是 Phase 4）：每个 PR 自动跑
  `bench_corpus.py`，与 main 分支的 baseline diff，**速度退化 > 5% 或失败率上升则阻塞合并**。
  - 实现：CI 缓存 baseline JSON，PR 跑完输出 diff 评论
  - 缓存 corpus 在 GitHub Actions cache，避免每次 clone PyMuPDF

### 性能预算

- corpus 平均速度退化 ≤ 5% per release
- corpus 失败率不上升
- char_sim 不退化（Phase 3 起量化跟踪）

### 版本控制策略（SemVer）

flashpdf 遵循 SemVer，但**按库而非按应用**解释：

- **PATCH（0.3.2 → 0.3.3）**：bug fix、性能优化、不增 API
- **MINOR（0.3.x → 0.4.0）**：新增功能 / 新增 API（向后兼容）
- **MAJOR（0.x → 1.0.0）**：破坏性变更——**目前没有计划**
- **0.x 阶段特殊规则**：在 1.0 之前，MINOR bump 可能含轻微不兼容，但会明确在 CHANGELOG 标注

何时升 1.0：Phase 1-2 全部完成，API 稳定 6 个月无破坏性变更，corpus 失败率稳定 < 1%。

### 向后兼容性策略

**核心原则：扩展而非修改。**

- `extract()` / `extract_many()` / `open()` 的现有签名永不变——新参数走 `**options` kwarg
- `TextBlock` / `TextSpan` 字典**只加字段不删字段**；老用户 `span["flags"]` 永远能解包
- 真要 deprecate：先标注 `# DEPRECATED since vX.Y`，下个大版本才删
- 例外：bug 修复导致输出变化（如错误的字符顺序）——这是 fix，不是 breaking

### Non-goals（明确不做的事）

避免用户对 flashpdf 有错误预期。**这些功能永远不做**——做的话应该用别的库：

| 功能 | 不做的原因 | 替代方案 |
|------|----------|---------|
| **页面渲染** (`get_pixmap()`) | 需要完整 PDF interpreter + 光栅化器，与"纯解析"设计相悖 | PyMuPDF / ritz / GoMuPDF |
| **OCR** | 需要训练模型 + GPU，与"轻量零依赖"目标相悖 | Tesseract / PaddleOCR（flashpdf 可输出图像字节供它们用） |
| **PDF 编辑 / 生成** | flashpdf 是 read-only 提取器 | reportlab / fpdf2 / PyPDF |
| **注释 / 表单填写** | 写操作需要完整 PDF 对象图，与提取目标正交 | PyMuPDF / pypdf |
| **矢量图光栅化** | 同"渲染"，需要光栅化器 | PyMuPDF |
| **GPU 加速** | JPEG/DEFLATE 硬解 ROI 不明，依赖链爆炸 | CPU 已经够快（corpus 平均 2.98ms） |

**判断标准**：任何新功能要回答"这要不要 GPU/渲染器/写操作"。要的话不做。

### Rust API 稳定性

flashpdf 当前**只承诺 Python API 稳定性**。Rust 侧（`flashpdf-core` crate）目前是
内部实现细节：

- `flashpdf-core` 0.x 期间，**struct 字段、函数签名、模块路径都可能变**
- 不推荐下游 Rust 项目直接依赖 `flashpdf-core`（除非接受跟随升级）
- **何时承诺 Rust API 稳定**：Phase 1-2 完成后，评估是否发布 `flashpdf-core` 1.0
  + 公开 Rust API 文档（用 rustdoc）
- 在 README / Cargo.toml description 明确标注"内部 crate，API 不稳定"

---

## 开放问题（待研究）

- **加密 AES-256 实际比例**：需先抽样统计真实 PDF 中 V5/R6 的占比，决定 2.1c 是否做
- **form XObject 渲染深度**：当前递归到 3 层，是否够用？需大语料验证
- **PDF 2.0 spec 兼容性**：未测试，需在 Phase 4 语料扩展时覆盖
- **PDF/UA 可访问性**：`/StructTreeRoot` + 标签化内容（`<Document>`/`<Part>` 等）是否
  值得解析？需要先调研 RAG/无障碍场景的需求量
- **AcroForm / XFA 表单提取**：fitz 能读 AcroForm 字段值；XFA 是 Adobe 私有 XML 格式
  复杂度极高。需要先看用户是否需要"读表单字段"功能
- **多语言支持**：CLI 输出 / 错误消息是否要 i18n？短期不做，看国际化反馈
- **LangChain / LlamaIndex / Haystack 集成**：是否提供官方 loader 插件？取决于采用率

---

## 决策日志

| 日期 | 决策 | 理由 |
|------|------|------|
| 2026-06-27 | 创建本 ROADMAP，取代 TODO.md | TODO.md 已是 v0.1.1 时代老文档，需重置 |
| 2026-06-27 | Phase 1 选 fitz 兼容而非精度 | 功能缺失比 92→99% char_sim 更影响用户采用 |
| 2026-06-27 | 加密 PDF 放 Phase 2 而非 Phase 1 | 风险更高、独立于 fitz 兼容主题 |
| 2026-06-27 | **不支持渲染/OCR/编辑**（写入 Non-goals） | 这些功能需要完全不同的技术栈（光栅化器/模型/写操作），与"纯解析"目标相悖；做的话应该用别的库 |
| 2026-06-27 | **不做 GPU 加速** | JPEG/DEFLATE 硬解 ROI 不明，依赖链爆炸；CPU 已达 2.98ms 平均 |
| 2026-06-27 | **先 fitz 兼容后精度** | 功能缺失（metadata/TOC/links）每个都是用户迁移的硬阻塞；char_sim 92→99% 是锦上添花 |
| 2026-06-27 | **CLI 用 Python click 不用 Rust clap** | 纯包装现有 API，0 跨语言复杂度；hot path 需要时再下沉 Rust（YAGNI） |
| 2026-06-27 | **AES-256 加密标为可选** | 复杂度跳跃（SASLprep/SHA-256 多轮），实际占比待抽样；先做 RC4 + AES-128 覆盖大多数场景 |
| 2026-06-27 | **Type3 仅走 ToUnicode 路径** | 渲染 Type3 字形需要光栅化器，违反 Non-goals；无 ToUnicode 的 Type3 标记为不可解 |
| 2026-06-27 | **char_sim 目标改为"单调增长 + 残差报告"** | 预先承诺 ≥97% 可能在根本性差异（fitz 输出 control char）前翻车；诚实目标更重要 |
| 2026-06-27 | **bench regression check 从 Phase 1 上线** | 性能预算每个 PR 都要守，不是 Phase 4 才自动化；避免回归悄悄累积 |
| 2026-06-27 | **Rust API 暂不承诺稳定** | `flashpdf-core` 是内部 crate，公开 rustdoc 推迟到 Phase 1-2 完成后 |
