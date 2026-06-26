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

## 四个阶段

| 阶段 | 主题 | 目标版本 | 核心交付 |
|------|------|---------|---------|
| 1 | fitz 功能补全 | v0.4.0 | `span.flags`、TOC、链接 API、CLI |
| 2 | 适用面扩大 | v0.5.0 | 加密 PDF、错误信息、examples、迁移指南 |
| 3 | 精度深挖 | v0.6.0 | Type3、竖排文本、char_sim 残差 |
| 4 | 规模化验证 | v0.7.0 | 扩语料、tiny 性能、logging、profile |

---

## Phase 1 — fitz 功能补全（v0.4.0）

**主题**：让"从 PyMuPDF 切到 flashpdf"成为无痛迁移。

**为什么先做**：每个功能都是 fitz 用户立即注意到的缺失。技术风险低、用户感知强、
互不阻塞——可以一周一个。

### 1.1 `span.flags` 格式探测

**Scope**：fitz 用 bitmask 编码 italic(2^1) / bold(2^4) / serif(2^2) / monospaced(2^3) /
superscript(2^0) 等。当前 `flags=0` stub。

- 解析 `/FontDescriptor >> /Flags`（按 PDF spec §7.9.2：
  bit 1=FixedPitch, bit 4=Symbolic, bit 6=Nonsymbolic, bit 7=Italic）
- `/BaseFont` 名称启发式匹配（`*Bold*`, `*Italic*`, `*Oblique*`, `*Mono*`, `*Courier*`）
- 装到 `TextSpan.flags: u32`，在 `emit_string` 时计算一次
- pyo3 侧暴露到 span 字典

**Verification**：
- 单元测试：构造含 `/FontDescriptor /Flags 32`（Italic）的 PDF，断言 `flags & 2^1 != 0`
- 回归：arxiv_2604 / dbnet_plus char_sim 不退化
- 对照：vs `fitz.open(p)[i].get_text("dict")` 的 flags，分类一致性 ≥ 95%

**风险**：Bold / Italic 探测在不同 fitz 版本下定义略有差异；先支持最稳的 4 位（italic/bold/serif/mono），
superscript 留到后续。

**复杂度**：中（解析已有，主要是 FontDescriptor 接入 + 启发式）

### 1.2 TOC / outline 提取

**Scope**：实现 `doc.get_toc()` → `[[level, title, page, dest?], ...]`，对齐 fitz。

- 解析 `/Root /Outlines` → `/First`/`/Next`/`/Title`/`/Dest`/`/A` 链表
- `page` 通过 `/Dest` 解引用到 page object → 页号
- 深度优先遍历，level 从 1 开始
- 处理损坏 outline（断链、循环引用）的回退

**Verification**：
- 单元测试：单层 outline / 多层嵌套 outline / 空 outline
- 对照：vs fitz `get_toc()` 在 5 个有目录的真实 PDF 上的输出
- corpus 不退化

**风险**：`/Dest` 命名引用 vs 显式引用两种语法都要支持；扫描件 PDF 偶有 page-ref 解析失败需 fallback。

**复杂度**：中（PDF spec §12.3 清晰，主要工作在 dest→page 解析）

### 1.3 链接提取 Python API

**Scope**：CHANGELOG 提到 Rust 侧 `extract_links` 已实现，但没接 pyo3。

- 暴露 `page.get_links()` → `[{"kind": "uri"|"goto"|"named", ...}]`
- 对齐 fitz `Link` 字段：`from` bbox, `kind`, `to` page, `uri`, ...
- 加到 `PyPage`

**Verification**：
- 对照 vs fitz 在带超链接的 arxiv PDF 上的输出
- 加 1 个单元测试覆盖 uri 链接 + 内部 goto

**风险**：低（核心逻辑已实现）

**复杂度**：低

### 1.4 CLI 工具

**Scope**：`flashpdf` 命令行入口，降低试用门槛。

```
flashpdf extract paper.pdf                    # 输出 text
flashpdf extract paper.pdf --mode dict > out.json
flashpdf extract *.pdf --output-dir out/      # 批量
flashpdf info paper.pdf                       # 页数、is_scanned 概览
flashpdf toc paper.pdf                        # 打印目录
```

- 用 `clap`（Rust 侧）或 Python `click`（最快路径）
- `maturin` 可以同时暴露 binary 和 module

**Verification**：
- 手动跑 5 个真实 PDF 验证输出
- README 加 CLI 章节
- `flashpdf --help` 自描述

**风险**：低（包装现有 API）

**复杂度**：低

### Phase 1 出口标准

- [ ] 1.1-1.4 全部完成且有单元测试
- [ ] `bench_corpus.py` 失败率仍 0%，速度退化 < 5%
- [ ] CHANGELOG v0.4.0 条目
- [ ] README + ROADMAP 更新
- [ ] PyPI 发布 + GitHub Release

---

## Phase 2 — 适用面扩大（v0.5.0）

**主题**：处理 fitz 能处理但 flashpdf 直接 fatal 的场景。

### 2.1 加密 PDF 支持

**Scope**：目前 `Document::from_mmap` 见到 `/Encrypt` 直接 fatal。支持常见场景：

- **RC4（V1-V2）**：用户/所有者密码，标准 PDF 1.5 加密
- **AES-128（V4）**：PDF 1.5+
- **AES-256（V5/R6）**：PDF 1.7 Extension Level 8（复杂，可选）
- 空 password 自动解密（大多数浏览器导出的"加密但无密码"PDF）

**Verification**：
- 单元测试：RC4 + AES-128 各一个加密 PDF（构造或从 PyMuPDF corpus 取）
- 加密 PDF 进 corpus 跑 `bench_corpus.py`，失败率仍接近 0%
- 错误密码明确报错，不 crash

**风险**：密码学代码要谨慎，建议用 `ring` 或 `aes` crate 而非自实现。RC4 简单，AES-256 涉及
SASLprep / SHA-256 多轮，复杂度跳跃。

**复杂度**：高（AES-256）/ 中（仅 RC4 + AES-128）

### 2.2 错误信息增强

**Scope**：当前错误都是 `Message("expected 'obj' keyword")`，没字节偏移、没 context。

- `ParseError` 改成结构体：`{ kind, offset: usize, context: Vec<u8>, msg }`
- Display 时输出：`error at byte 12345: expected 'obj' keyword\n  context: ... "0000000107 00000 n" ...`
- 加 `error_chain` 让上层错误保留原始 cause

**Verification**：
- 故意损坏的 PDF 报错带 offset
- 不影响 corpus 失败率（只是文案改）

**风险**：低（纯重构）

**复杂度**：低

### 2.3 examples/ 目录

**Scope**：给"我要做 X 该怎么写"提供 copy-paste 起点。

- `examples/rag_index.py`：批量 PDF → JSON → embedding
- `examples/markdown_export.py`：dict → Markdown（标题/段落/list 启发式）
- `examples/ocr_bridge.py`：扫描页 → 图像字节 → Tesseract
- `examples/toc_to_yaml.py`：`get_toc()` → YAML

**Verification**：每个 example 在 1-2 个真实 PDF 上能跑

**风险**：无

**复杂度**：低

### 2.4 fitz 迁移指南

**Scope**：`docs/MIGRATION_FROM_FITZ.md`，覆盖常见差异。

- API 对照表（已有的+扩展）
- 输出格式差异（flags / ascender / 等）
- 不支持的功能（rendering / annotation）的替代方案
- 性能 / 稳定性差异的预期

**Verification**：让 1 个 fitz 重度用户试读，看能否独立迁移

**风险**：无

**复杂度**：低

### Phase 2 出口标准

- [ ] 加密 PDF 支持（至少 RC4 + AES-128）
- [ ] 错误信息带 offset
- [ ] 4 个 examples 可跑
- [ ] 迁移指南
- [ ] corpus 失败率 ≤ 1%（加入加密文件后允许少量硬失败）
- [ ] PyPI 发布

---

## Phase 3 — 精度深挖（v0.6.0）

**主题**：把 char_sim 从 92-93% 推到 99%+，消除剩余边缘 case。

**为什么放第三阶段**：92% 已可读，剩余 7% 多为 Type3 / 竖排 / 复杂版式，技术深度高、用户感知弱。
等 1/2 阶段把"功能层面追平 fitz"后，再做这种"超越 fitz"的优化。

### 3.1 Type3 字体专门处理

**Scope**：`/Type3` 字体的字形由绘图算子定义，不走标准字体路径。

- 检测 `/Subtype /Type3`，触发专门 handler
- 解析 `/CharProcs` 字典 → 每个字符的 content stream
- 选项 A：把 Type3 字形渲染成 bbox + 标识符（保留可检索性，不渲染视觉）
- 选项 B：调用 `/FontMatrix` + 字形 content stream 拿到真实 bbox，结合 `/ToUnicode` 输出字符

**Verification**：
- 找 2-3 个 Type3 PDF（PyMuPDF corpus 有 `type3font.pdf`）
- 对照 fitz 输出，字符数误差 < 5%
- `diagnostics.type3_char_count` 在主路径上下降

**风险**：Type3 是 PDF 里最诡异的字体类型；某些 Type3 字体没有 `/ToUnicode`，只能 OCR。

**复杂度**：高

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

**Scope**：找出 char_sim 与 fitz 差 7% 的具体来源。

- 写脚本 diff flashpdf vs fitz 输出，分类残差（whitespace / order / missing chars / extra chars）
- 按分类逐个收敛
- 目标：char_sim 95% → 99%

**Verification**：
- 残差分析报告 `docs/CHAR_SIM_AUDIT.md`
- 每修一类，char_sim 单调上升

**风险**：可能发现根本性差异（如 fitz 输出特定 control char），无法完全消除。

**复杂度**：中（取决于残差类型）

### Phase 3 出口标准

- [ ] Type3 至少选项 A 路径走通
- [ ] 竖排文本可读流利
- [ ] char_sim ≥ 97%
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

加 `tests/bench_corpus_extended.py` 跑多语料聚合。

**Verification**：3 个语料合起来 1500+ PDF，flashpdf 失败率 < 5%（接受少量硬失败，但每个有诊断）

**风险**：可能暴露大量边缘 case，需要分类排优先级。

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

- [ ] 至少 2 个新语料集成到 bench
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
- 加 bench regression check（速度退化 > 5% 阻塞合并）——可选，Phase 4 落地

### 性能预算

- corpus 平均速度退化 ≤ 5% per release
- corpus 失败率不上升
- char_sim 不退化

---

## 开放问题（待研究）

- **加密 AES-256 是否值得做**：实际比例多大？需先抽样统计。
- **GPU 加速**（TODO.md 历史项）：JPEG 硬解 / DEFLATE 硬解 ROI 不明，暂搁。
- **form XObject 渲染深度**：当前递归到 3 层，是否够用？需大语料验证。
- **PDF 2.0 spec 兼容性**：未测试，需在 Phase 4 语料扩展时覆盖。

---

## 决策日志

| 日期 | 决策 | 理由 |
|------|------|------|
| 2026-06-27 | 创建本 ROADMAP，取代 TODO.md | TODO.md 已是 v0.1.1 时代老文档，需重置 |
| 2026-06-27 | Phase 1 选 fitz 兼容而非精度 | 功能缺失比 92→99% char_sim 更影响用户采用 |
| 2026-06-27 | 加密 PDF 放 Phase 2 而非 Phase 1 | 风险更高、独立于 fitz 兼容主题 |
