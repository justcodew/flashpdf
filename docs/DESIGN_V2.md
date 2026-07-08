# flashpdf — 自研极致性能 PDF 提取引擎方案书

> **项目定位**：从零构建全球最快的 PDF 文本与图像提取引擎，输出与 PyMuPDF 兼容的 `blocks` 与 `images`，不牺牲任何精度，支持多页/多文件并行及可选 GPU 加速。

---

## 1. 技术选型

| 类别 | 选型 | 说明 |
|------|------|------|
| PDF 解析器 | **自研最小化零拷贝解析器** | 跳过通用库抽象，直接在 mmap 字节上工作 |
| 内容流扫描 | `memchr` (SIMD) | 快速定位操作符 `BT` `ET` `Tj` `TJ` `Do` |
| 数字解析 | `fast-float` | 比标准库快 2-3 倍的浮点转换 |
| 字体缓存 | `FnvHashMap` + `Vec<f32>` 直索引 | CID→宽度 O(1) 访问，子集化加载 |
| 临时存储 | `SmallVec` | 减少聚类过程中的堆分配 |
| PNG 编码 | `zune-png` | 更快的 DEFLATE 实现（惰性调用） |
| 并行框架 | `rayon` | 页面级与文件级并行 |
| Python 绑定 | `pyo3` | 释放 GIL，导出原生 Python 对象 |
| I/O | `memmap2` | 零拷贝文件映射 |
| GPU (可选) | `nvjpg` / `nvcomp` (FFI) | NVIDIA GPU 解码/解压，feature flag 控制 |

---

## 2. 自研 PDF 解析器设计

### 2.1 设计原则
- **零拷贝**：所有操作基于 `&[u8]` 引用，直接在 mmap 区域解读。
- **最小必要**：仅实现提取所需的对象解析、xref 解析、流解压，无 DOM 构建。
- **健壮容错**：主路径严格按标准，fallback 路径容忍常见破损。

### 2.2 实现范围（核心 ~800 行）
- **对象解析器** (~250 行)：递归下降解析 int/real/string/name/array/dict/stream/ref。
- **标准 xref 表** (~150 行)：解析文本格式 xref 条目。
- **xref 流** (~200 行)：处理 PDF 1.5+ 压缩交叉引用，支持 `/W` `/Index`。
- **trailer 与链式 xref** (~80 行)：提取 `/Root` `/Size` `/Prev`。
- **对象定位** (~120 行)：利用偏移直接 seek，`memchr` 界定 `obj`/`endobj` 边界。
- **对象流 (ObjStm)** (~80 行)：解压并索引内嵌对象。

### 2.3 明确排除
加密、线性化、数字签名、增量更新合并（仅处理最后一份 xref）、对象流内间接引用的完全递归。

### 2.4 容错策略
- **主路径**：使用标准 xref 偏移直接定位（O(1)）。
- **降级路径**：xref 损坏时自动 `memchr` 全文扫描所有 `obj` 标记，重建偏移表，覆盖老旧/脏 PDF。

---

## 3. 系统架构

```
mmap 文件 ──→ 自研解析器 (顺序: trailer → xref → 构建偏移表)
                     │
                     ├─ 页面快照 (Arc<[u8]> 零拷贝内容流)
                     │    └─ rayon 并行: 多页内容流同时解析
                     │
                     ├─ 全局字体缓存 (子集化 CID→宽度)
                     │
                     ├─ 内容流解析 (SIMD 扫描 + fast-float)
                     │    ├─ Form XObject 递归 (深度 3)
                     │    └─ Do 图像捕获 (存储 mmap 偏移+长度)
                     │
                     ├─ 在线布局聚类 (可选 feature)
                     │
                     └─ 图像提取
                          ├─ JPEG/JPX → mmap 切片零拷贝
                          ├─ Flate/LZW → 惰性 PNG (zune-png)
                          └─ GPU 加速 (可选, nvjpg/nvcomp)
```

---

## 4. 性能优化三层路线

### 第一层：微观优化（默认开启）
- `memchr` SIMD 加速操作符定位 (2-5x)
- `fast-float` 浮点解析 (2-3x)
- `FnvHashMap` + `Vec<f32>` 宽度直索引
- `SmallVec` 减少聚类分配

### 第二层：架构优化
- 全链路零拷贝（mmap → 快照 → 图像切片）
- 字体子集化加载
- 文件级异步预读
- 在线聚类（可选，内存 -30%，延迟更低）

### 第三层：极限优化
- 自研解析器已是该层核心
- GPU 加速编解码 (数量级提升)

---

## 5. 输出 API 与兼容性

```python
import flashpdf

# 单文档
blocks, images = flashpdf.extract(
    "doc.pdf",
    page_parallel=True,
    include_images=True,
    gpu=False
)

# 批量流式
for path, blocks, images in flashpdf.extract_many(
    paths,
    file_parallel=True,
    page_parallel=False,
    include_images=False,
    gpu=False
):
    ...
```

- `blocks` 结构：`Block → Line → Span → Char`，字段与 PyMuPDF 完全一致。
- `images` 列表：`bbox, width, height, bpc, colorspace, xref, ext, image`，`image` 为零拷贝 mmap 切片或惰性 PNG。

---

## 6. 开发计划（总计约 23 天）

### 阶段 1a：对象解析器（2 天）
- [ ] 递归下降解析 int/real/string/name/array/dict/stream/ref
- [ ] 流对象 `/Length` 读取与边界处理
- [ ] **交付标准**：能解析 5 个不同 PDF 的所有对象类型，输出与 lopdf 对比一致

### 阶段 1b：xref + trailer + ObjStm（2 天）
- [ ] 标准 xref 表解析、trailer 解析 `/Root` `/Size` `/Prev` 链式追溯
- [ ] xref 流解析（`/W` 字段、`/Index` 数组）
- [ ] 对象流 (ObjStm) 解压与内嵌对象索引
- [ ] **交付标准**：能正确打开 10+ PDF，包括有 xref 流的

### 阶段 1c：memchr fallback + 容错（1 天）
- [ ] xref 偏移损坏时 ±N 字节搜索 obj 校正
- [ ] memchr 全文扫描建表作为最终降级
- [ ] **交付标准**：故意损坏 xref 的 PDF 仍能打开并提取内容

### 阶段 1d：集成页面快照 + 性能基准（2 天）
- [ ] 页面快照系统 (Arc<[u8]> 零拷贝内容流)
- [ ] 性能基准对标 lopdf（同一批 PDF，对比解析耗时）
- [ ] **交付标准**：提取页面内容流，速度 ≥ lopdf

### 阶段 2a：内容流解析基础版（2 天）
- [ ] 宽容状态机，集成 `memchr` 和 `fast-float`
- [ ] 标准字体支持，`FnvHashMap` 缓存
- [ ] 字符定位与 bbox，预留 Form 递归

### 阶段 2b：字体扩展（条件触发，2 天）
- [ ] CMap 最小实现，Type0 降级，预计算数组
- [ ] 触发条件：2a 后文本丢失率 >5%

### 阶段 3：布局分析（2 天）
- [ ] 两遍法聚类 (SmallVec)
- [ ] 在线聚类 feature（可选）

### 阶段 4：图像提取与 GPU（2 天）
- [ ] 零拷贝图像偏移记录，四角变换 bbox
- [ ] 惰性 PNG (zune-png)
- [ ] GPU 集成（nvjpg/nvcomp, feature flag）

### 阶段 5：并行化与 I/O 优化（2 天）
- [ ] 页面/文件并行，GIL 释放
- [ ] 异步预读，大文档自动分批保护 (FLASHPDF_BATCH_SIZE)

### 阶段 6：性能验证与测试（3 天）
- [ ] flamegraph 热点消除，criterion 基准
- [ ] 20+ 样本 PyMuPDF/pypdfium2 对比 diff
- [ ] GPU 路径正确性验证

#### 测试集规格（从阶段 1 开始收集，贯穿全程）
| 类型 | 数量 | 验证目标 |
|------|------|----------|
| 纯文本论文 | 5 | 文本提取精度、bbox 准确性 |
| 图文混排杂志 | 5 | 布局聚类 + 图像提取 |
| 扫描件纯图像 | 3 | 图像提取、无文本时不报错 |
| 表格密集型 | 3 | 字符间距、bbox 精度 |
| 中日韩文档 | 3 | 字体 CMap、CID 解码 |
| xref 损坏/老旧 | 2 | memchr fallback 容错 |

### 阶段 7：文档与发布（1 天）
- [ ] README，API 文档，wheel 构建

---

## 7. 成功指标

- **速度**：文本提取 ≥ PyMuPDF 2x，图像元数据提取（仅记录偏移）≥ 50x，图像字节提取（含解码）≥ 5x，多文件吞吐量近核心数线性增长。
- **兼容**：输出与 PyMuPDF 结构差异 <2%，bbox 平均误差 <1pt，文本一致性 >99.9%。
- **健壮**：千份随机 PDF 零崩溃，格式异常自动降级。

---

此方案已移除对任何通用 PDF 库的依赖，用 **800 行自研解析器**换取全链路零拷贝和绝对性能掌控，是构建全球最快 PDF 提取引擎的完整蓝图。
