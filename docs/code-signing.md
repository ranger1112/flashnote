# 代码签名调研（Windows / SmartScreen）

本文回答一个问题：**闪记要不要买代码签名，买哪种，怎么接进现有的 Tauri + GitHub Actions 发布流程。**

结论先行：

| 方案 | 首年成本量级 | CI 无人值守 | SmartScreen 首次下载 | 建议 |
| --- | --- | --- | --- | --- |
| 暂不签名（现状） | 0 | 不涉及 | 拦截，需「更多信息 → 仍要运行」 | **当前阶段采用** |
| Azure Artifact Signing（原 Trusted Signing） | 约 $10/月起 | 支持，客户端密钥即可 | 仍会提示一次，但显示发布者名称，信誉随版本累积 | **如果要签名，选这个** |
| 传统 OV 证书 | 约 $200–500/年 + 令牌/云 HSM | 需要云 HSM，接入成本高 | 同样需要积累信誉 | 不建议个人项目自购 |
| EV 证书 | 约 $400–800/年 + 令牌 | 同上 | **2024 年起不再自动豁免** | 不建议 |

> 价格随渠道和汇率波动，以上只用于量级判断；以各厂商当期报价为准。

---

## 1. SmartScreen 实际拦的是什么

Windows 的 SmartScreen 在用户运行「从网络下载的文件」时看两个信号（[Microsoft 文档](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)）：

1. **发布者信誉**：文件有没有签名、证书是不是已知发布者；
2. **文件哈希信誉**：这个具体文件有没有被足够多用户下载并正常使用过。

由此有几个常被误解的点：

- **签名 ≠ 立刻没有提示。** 新签名的新文件依然可能提示「无法识别的应用」，直到该哈希积累足够的下载记录。微软的说法是需要数周、数百次来自广泛用户的干净安装。
- **不签名的文件，每个新版本的信誉从零开始。** 上一个版本攒下的信任不会转移，因为哈希变了、也没有发布者身份。
- **签名后新版本有机会继承证书信誉。** 同一张证书持续签名，新文件更容易被放行——前提是每次发版都签。
- **EV 证书自 2024 年起不再自动获得 SmartScreen 信任。** 微软文档明确写道：过去 EV 证书默认带来正面信誉，这个行为已不存在，「仅为绕过 SmartScreen 而支付 EV 溢价已不再合理」。EV 剩下的价值主要是部分企业的采购要求。
- **Windows 11 的 Smart App Control 更严格**：未签名（或没有正面信誉）的可执行文件可能直接阻止运行，而不只是提示。这会随 Windows 11 装机量增长影响更多用户。
- **企业环境可能完全禁止绕过提示**，那时未签名安装包等于不可用。

对本项目而言：安装包只发一次，之后升级走应用内自动更新（不经过 SmartScreen 的下载拦截逻辑），所以签名的收益集中在**首次安装**这一段。这也是目前唯一的体验缺口——README 里只能教用户点「仍要运行」。

---

## 2. 和自动更新签名是两套独立机制

这是最容易混淆的地方，必须分开看：

| | Authenticode（代码签名） | minisign（更新签名） |
| --- | --- | --- |
| 目的 | 向 Windows / 用户证明「这个 exe 是谁发布的、下载后没被改过」 | 向应用证明「这个更新包来自本项目，没被替换」 |
| 验证者 | Windows（SmartScreen、UAC、Smart App Control） | `tauri-plugin-updater`（内置公钥） |
| 密钥 | 证书私钥，由 CA / 云签名服务保管 | `TAURI_SIGNING_PRIVATE_KEY`（minisign 私钥） |
| 配置位置 | `bundle.windows.signCommand` | `plugins.updater.pubkey` + `createUpdaterArtifacts` |
| 现状 | **未启用** | **已启用**，v0.2.0 → v0.2.1 已实测通过 |

要点：

- **两者互不替代。** 只做 Authenticode，自动更新仍会校验 minisign 签名，`TAURI_SIGNING_PRIVATE_KEY` 不能删；反过来，minisign 签名对 SmartScreen 毫无作用。
- **Authenticode 不参与更新验证。** 更新包是不是微软认可的发布者签的，updater 不关心；它只认 `latest.json` 里那个 minisign 签名。因此现在这套自动更新在未签名的情况下也能正常工作（已在 0.2.0 → 0.2.1 上完整验证：检测、下载、验签、安装、重启）。
- **顺序很重要。** Tauri 在打包阶段通过 `signCommand` 对主程序、资源、NSIS 安装包签名，更新用的 `.sig` 是在最终产物上生成的。**不要在 `tauri build` 之后手动改文件或重新签名**，否则 `.sig` 与文件不再匹配，所有用户都会更新失败；如果确实要后置签名，必须同时重新生成 `.sig` 和 `latest.json`。

---

## 3. 方案对比

### 3.1 暂不签名（当前方案）

- 成本 0，不增加 CI 依赖，不暴露任何身份信息。
- 用户首次安装会遇到 SmartScreen 蓝屏提示；README/FAQ 已写明「更多信息 → 仍要运行」，并提供 `SHA256SUMS.txt` 供校验。
- 对早期项目可接受：用户量小，且升级路径（自动更新）不受影响。
- 风险：Smart App Control 普及后，部分 Windows 11 用户可能完全无法运行；企业环境可能直接封锁；新用户流失无法量化。

### 3.2 Azure Artifact Signing（原 Azure Trusted Signing）

微软面向非 Store 分发推荐的托管签名服务（[概述](https://learn.microsoft.com/en-us/azure/artifact-signing/overview)、[FAQ](https://learn.microsoft.com/en-us/azure/artifact-signing/faq)）。

- **证书不在你手里**：私钥保存在 FIPS 140-3 Level 3 的 HSM 中，签名时只上传文件摘要，证书不会下发，也没有硬件令牌。
- **定价**：Basic 档起价 $9.99/月，含每月 5,000 次签名；Premium 档含 100,000 次。费用**不按比例折算**，开通当月即按整月计费。必须使用**付费 Azure 订阅**，免费/试用/赞助订阅不能创建账户。
- **身份验证**：个人可走 Individual validation，需要政府签发的带地址证件，或银行/水电账单 + Microsoft Authenticator 的 Verified ID FaceCheck。验证有有效期，过期不续会停止签发新证书。
- **不签发 EV 证书**，也不支持自定义 CN/O——证书主体必须是验证过的法定名称。**对个人开发者意味着发布者名称会显示你的真实姓名**，这与「公开仓库不出现个人信息」的取舍需要提前想清楚。
- **SmartScreen**：首次下载仍可能提示，但会显示经过验证的发布者名称；持续用同一身份签名，信誉会跨版本累积。
- **CI 友好**：官方推荐的接入方式就是服务主体 + 客户端密钥，GitHub Actions 原生支持。

### 3.3 传统 OV 证书

- 需要向 CA（DigiCert、Sectigo、Certum 等）购买，通常 $200–500/年。
- 2023 年 6 月起 CA/B Forum 要求私钥存放在硬件令牌或云 HSM 中，不能再以文件形式导入。CI 里签名要么接云 HSM（如 Azure Key Vault + relic 之类的工具），要么在自托管机器上插令牌——**无人工干预的难度明显高于 Artifact Signing**。
- SmartScreen 行为与 Artifact Signing 相同：显示发布者，但首次下载仍需积累信誉。
- 适合已有企业主体、需要证书自持的场景；对单人开源项目性价比低。

### 3.4 EV 证书

- 价格最高，同样需要令牌/HSM，CI 接入与 OV 一样麻烦。
- **不再豁免 SmartScreen**（见第 1 节），因此对本项目几乎没有额外收益。
- 只有在企业客户明确要求 EV 时才值得考虑。

### 3.5 其他可能：SignPath Foundation（开源免费）

SignPath 面向符合条件的开源项目提供免费的托管签名，证书由 SignPath Foundation 持有，通过 GitHub Actions 集成；申请条件是 OSI 认可的开源许可证、公开活跃仓库，并接受其构建/签名流程审核。需要注意：

- 发布者显示的是 SignPath Foundation（项目名通常作为 CN），不是闪记自己；
- 多个项目共用同一签名主体，信誉不完全等同于独立证书；
- 申请需要审核和一定的接入改造，条款以官网当期说明为准。

如果不想承担月费、又不想暴露个人姓名，这是值得评估的第三条路；但它的审核与接入成本高于「继续不签名」，低于「自购 OV」。

---

## 4. Azure Artifact Signing 接入步骤（备选路径）

以下步骤在决定采购后执行，当前仓库**尚未启用**。

### 4.1 Azure 侧

1. 准备**付费** Azure 订阅，在订阅的「资源提供程序」中注册 `Microsoft.CodeSigning`。
2. 创建 Artifact Signing 账户，记下账户名与区域端点（形如 `https://wus2.codesigning.azure.net`）。
3. 完成身份验证（个人选 Individual），创建 **Public Trust** 证书配置文件（Basic 档每种类型 1 个）。
4. 在 Microsoft Entra 中创建应用注册，生成客户端密钥，并授予该应用 **Artifact Signing Certificate Profile Signer** 角色（作用域为签名账户/资源组）。
5. 把 `AZURE_CLIENT_ID`、`AZURE_CLIENT_SECRET`、`AZURE_TENANT_ID` 存为仓库 Actions secrets。客户端密钥有有效期，需要定期轮换。

### 4.2 Tauri 配置

```json
{
  "bundle": {
    "windows": {
      "signCommand": "artifact-signing-cli -e https://wus2.codesigning.azure.net -a <account> -c <profile> -d FlashNote %1"
    }
  }
}
```

- `%1` 由 Tauri 替换为待签名文件路径，主程序、资源文件和 NSIS 安装包都会走这个命令。
- `-d` 是可选描述，主要影响 MSI 的 UAC 提示。
- CLI 来自 [`Levminer/artifact-signing-cli`](https://github.com/Levminer/artifact-signing-cli)（`cargo install artifact-signing-cli`），依赖 .NET 8、Azure CLI 与 Windows 11 SDK 中的 `signtool`。

### 4.3 GitHub Actions

在现有 `release.yml` 的 tauri-action 步骤上补充环境变量即可，其余流程（minisign 签名、`latest.json`、`SHA256SUMS.txt`）不变：

```yaml
- name: Install signing CLI
  run: cargo install artifact-signing-cli

- uses: tauri-apps/tauri-action@v0
  env:
    TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
    AZURE_CLIENT_ID: ${{ secrets.AZURE_CLIENT_ID }}
    AZURE_CLIENT_SECRET: ${{ secrets.AZURE_CLIENT_SECRET }}
    AZURE_TENANT_ID: ${{ secrets.AZURE_TENANT_ID }}
  with:
    # 现有参数保持不变
    args: --target x86_64-pc-windows-msvc
```

`windows-latest` 自带 .NET 与 Windows SDK；若 CLI 编译时间不可接受，可改为下载其 release 中的预编译二进制。

### 4.4 验证

构建完成后在本地或 CI 里检查安装包签名：

```powershell
signtool verify /pa /v src-tauri\target\release\bundle\nsis\闪记_<版本>_x64-setup.exe
```

预期看到 `Successfully verified`，且时间戳来自 `timestamp.acs.microsoft.com`。同时确认 Release 里的 `.sig` 仍由 tauri-action 生成、`latest.json` 指向新安装包——签名不能破坏更新链路。

---

## 5. 推荐

**短期（现在）：保持不签名。**

- 自动更新的完整性与来源校验已经由 minisign 覆盖，安全层面没有缺口；签名解决的是首次安装的信任展示。
- 当前用户规模下，$10/月 + 真实姓名作为发布者 + 身份审核的代价，收益还不成立。
- README/FAQ 已明确说明 SmartScreen 提示与 SHA256 校验方式，这是现阶段合理的「已知限制」。

**触发条件（满足任一即可重新评估）：**

- 有非技术用户反馈因 SmartScreen 放弃安装，或下载量明显高于安装量；
- 需要进入企业环境，或被 Smart App Control 拦截的反馈出现；
- 项目获得赞助/商业化，需要正式发布者身份。

**届时选择：Azure Artifact Signing Basic + Individual validation。** 它不需要硬件令牌、CI 接入最简单、按签名次数计费（每月 5,000 次远超本项目发版频率），且是微软当前对非 Store 分发的推荐路径。不建议自购 OV/EV：OV 的 CI 成本更高，EV 已不再豁免 SmartScreen。

若不愿承担月费或不愿公开真实姓名，先评估 SignPath Foundation 的开源计划；两条路都不合适时继续维持现状，并在 Release 说明中保留 SmartScreen 提示。

---

## 6. 后续接入检查清单

- [ ] 确认 Azure 付费订阅与账单负责人
- [ ] 完成 Individual（或 Organization）身份验证，确认发布者名称可接受
- [ ] 创建账户 + Public Trust 证书配置，记录端点/账户/配置名
- [ ] 创建服务主体，授予 Certificate Profile Signer 角色，设置 secrets
- [ ] 在 `tauri.conf.json` 添加 `signCommand`，本地构建并用 `signtool verify` 验证安装包
- [ ] 在 `release.yml` 注入三个 Azure 环境变量，跑一次 tag 构建
- [ ] 验证自动更新链路：旧版本能检测、下载、校验、安装新版本
- [ ] 更新 README/FAQ：说明已签名，但首次下载仍可能提示
- [ ] 记录证书主体与续期时间（身份验证到期会停止签发）

---

## 7. 参考

- [SmartScreen reputation for Windows app developers](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)
- [What is Azure Artifact Signing?](https://learn.microsoft.com/en-us/azure/artifact-signing/overview)
- [Azure Artifact Signing FAQ](https://learn.microsoft.com/en-us/azure/artifact-signing/faq)
- [Azure Artifact Signing pricing](https://azure.microsoft.com/pricing/details/artifact-signing/)
- [Tauri v2：Windows 代码签名](https://v2.tauri.app/distribute/sign/windows/)
- [`artifact-signing-cli`](https://github.com/Levminer/artifact-signing-cli)
- [SignPath](https://signpath.io/)
