# Hakoniwa Desktop Manager

**Hakoniwa Core と個別アセットの導入・起動・停止・監視、ならびにアセット間の通信状態を一つのローカル画面で扱うクロスプラットフォーム・デスクトップアプリケーション**です。Windows、macOS、Linuxを配布対象とし、WindowsホストからのWSL2アセット起動にも対応します。

Hakoniwa Core Proは、共有メモリ上のmaster dataとPDU bufferを用いてアセットのPDU通信と時刻同期を実現するランタイムです。[1] このアプリは、OSプロセスの起動状態、`hako-cmd`が制御するシミュレーション時刻、Endpoint／Bridgeで観測される通信状態を明確に分離して表示します。

> **安全設計**：任意のシェル文字列を画面からそのまま実行しません。アセットは「実行ファイル」「引数」「作業ディレクトリ」「環境変数」「実行環境」に構造化して保存します。Coreを導入する場合は、HTTPS、SHA-256照合、危険なアーカイブパスの拒否、アトミックな展開を必須とします。

## 機能

| 領域 | 初期版の機能 |
| --- | --- |
| **Core管理** | 承認済みのOS別バイナリカタログ、SHA-256検証済みダウンロード、展開、バージョン選択、`hako-cmd start/stop/reset`実行を提供します。 |
| **Core実行** | Conductor所有アセット、Launcher、または任意のCoreコントローラーを構造化コマンドとして起動・停止し、PID、稼働時間、終了コード、標準出力／標準エラーを監視します。 |
| **アセット管理** | アセットを画面から追加、編集、複製相当の保存、削除、個別起動、個別停止、一括起動、一括停止できます。依存関係と`before_start`／`after_start`を保持します。 |
| **WSL2** | Windows上では`wsl.exe -d <distribution> --cd <cwd> -- env ...`を使い、明示したディストリビューション内でアセットを起動します。 |
| **Business Pack取込** | Launcherの`assets[]`、Endpoint設定、Bridge設定を再帰的に検出して、アセット・PDU／通信経路のプレビューを作成します。元のRecipe設定は書き換えません。 |
| **通信可視化** | Endpoint／Bridge設定から抽出したノードと経路を表示し、`Connected`、`Idle`、`Disconnected`、`Unknown`を通信イベントに基づいて判定します。 |
| **通信詳細** | 経路ごとの最終活動時刻、送受信件数・バイト数、PDU名、構成由来、エラー、イベント時系列を表示します。PDUペイロードは標準では保存しません。 |

## 要件と位置付け

Business Packでは、`hakoniwa-core-pro`、PDU Endpoint、PDU Bridge Core、PDU Pythonが中核コンポーネントとして整理されています。[2] EndpointはSHM、TCP、UDP、WebSocket、Zenoh、MQTTなどの通信方式を設定で扱い、Bridge Coreは経路・転送タイミング・monitor機能を提供します。[3] [4]

そのため初期版は、**Core Proのライフサイクル**と、**Endpoint／Bridgeで明示された通信経路**を優先してサポートします。ローカル共有メモリはOSのネットワーク接続一覧だけでは通信実態を判定できません。Core Proの公開データ受信イベントまたは読み取り専用monitorアセットを将来の拡張点として分離し、初期版はBridge monitorとEndpoint／Bridgeの構成・ログから得られる観測情報を利用します。[1] [4]

| 監視対象 | 初期版の観測方法 | 表示できること | 制約 |
| --- | --- | --- | --- |
| Coreコントローラー／アセット | アプリが管理する子プロセス | PID、状態、終了コード、稼働時間、ログ末尾 | アプリ外から起動した任意プロセスの完全な制御はしません。 |
| `hako-cmd` | 承認済みCoreの`hako-cmd` | start／stop／resetの結果 | OSプロセスの終了操作とは別です。 |
| Endpoint／Bridge構成 | JSONの静的解析 | 送信元、宛先、transport、PDU定義、Bridge設定 | 実行中の通信を直接保証するものではありません。 |
| Bridge monitor／Endpointログ | 構造化または行単位のイベント取込 | 接続状態、通信数、PDU、最終活動、エラー | 対応するmonitor／ログ出力が必要です。 |
| 遠隔アセット | Endpoint／Bridge／RPC経路 | 接続とハートビート／データフロー | リモート側に観測用endpoint・bridge・テレメトリがなければ、到達性の範囲に限定されます。 |

## Windowsでのダブルクリック起動

`tools\launch-windows.cmd` をダブルクリックすると、Windowsネイティブの実行ファイルが起動します。ソースが実行ファイルより新しければ自動で再ビルドし、`start`で切り離して起動するのでコンソールは残りません。

| 前提 | 内容 |
| --- | --- |
| Rust | rustupで導入したMSVCツールチェーン（`x86_64-pc-windows-msvc`）。 |
| C++ビルドツール | Visual Studio 2022のC++ワークロード、またはBuild Tools。 |
| Node.js | フロントエンドのビルドに使います。 |
| WebView2ランタイム | Windows 11には既定で入っています。 |

初回はビルドに数分かかります。Rustのコードを変更したときに強制的に作り直すには、環境変数`HDM_REBUILD=1`を付けて起動します。

インストーラを使う場合は、`pnpm tauri build`が生成する次のどちらかを実行してください。スタートメニューとデスクトップにショートカットが作られます。

```text
src-tauri/target/release/bundle/nsis/Hakoniwa Desktop Manager_0.1.0_x64-setup.exe
src-tauri/target/release/bundle/msi/Hakoniwa Desktop Manager_0.1.0_x64_ja-JP.msi
```

WSL上のWSLg経由でも起動できますが、環境によってはWSLgがGPU共有に失敗して`COPY MODE`へ落ち、ウィンドウが真っ黒のまま何も描画されないことがあります（`MESA: error: ZINK: failed to choose pdev`）。この症状が出る環境ではネイティブビルドを使ってください。

### 既知の問題

起動直後にウィンドウが最小化状態で現れることがあります。タスクバーのアイコンから復元してください。原因は未特定です。

## 開発環境での起動

### 前提条件

| 対象 | 必要なもの |
| --- | --- |
| 全OS | Node.js 22以降、pnpm、Rust stable、OS向けTauri開発依存関係 |
| Linux | `libgtk-3-dev`、`libwebkit2gtk-4.1-dev`、`libayatana-appindicator3-dev`、`librsvg2-dev` |
| Windows | Visual Studio Build Tools（C++）、WebView2、PowerShell |
| macOS | Xcode Command Line Tools |
| Hakoniwa Recipe | 選択したCore Pro、対応するPDU定義、必要に応じてPython 3.12と`hakoniwa-pdu` |

```bash
pnpm install
pnpm tauri dev
```

本アプリのLinux開発環境では、次の検証を実施済みです。

```bash
pnpm run check
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

Windows、macOS、Linuxの配布物は、それぞれ対応するホストOS上のCIまたはリリース環境で生成してください。異なるOSのGUI実行ファイルをLinuxから汎用にクロスコンパイルすることは、本プロジェクトのリリース手順に含めません。

## 典型的な利用手順

1. **Core管理**画面で、承認済みカタログに登録されたCore Proを検証付きで導入します。
2. **Coreコントローラー**として、Conductor所有プロセスまたはLauncherを登録します。`hako-cmd`自身を長時間実行するコントローラーとして登録しないでください。
3. **取込・設定**画面からBusiness PackのRecipeまたは設定ディレクトリを選び、検出結果を確認して適用します。
4. **アセット**画面で、個別アセットのコマンド・引数・依存関係・実行環境を確認し、必要なものを追加します。
5. 一括起動または個別起動を行います。`before_start`アセットを起動後、Coreを`hako-cmd start`で開始し、`after_start`アセットを起動します。
6. **接続・通信**画面で、Endpoint／Bridgeの経路と直近の通信状態を確認します。経路を選択すると、件数、バイト数、PDU、イベント、構成由来を確認できます。

Hakoniwaの通常のランタイムでは、アセットを登録して待機させた後で`hako-cmd start`によりシミュレーション時間を開始します。プロセスを起動しただけでは、シミュレーションおよびPDU通信が進行しているとは限りません。[5]

## 承認済みCoreカタログ

このアプリが導入対象とするCoreは [`hakoniwalab/hakoniwa-core-pro`](https://github.com/hakoniwalab/hakoniwa-core-pro) です。[1] アーティファクトは同リポジトリの指定コミットから[`publish-core-artifacts.yml`](.github/workflows/publish-core-artifacts.yml)でビルドし、SHA-256とともに承認カタログへ登録します。カタログの各エントリは`provenance`に元リポジトリ・リリースタグ・ビルドワークフローを記録します。

アプリは、実行時に次の場所のローカルカタログだけを利用します。

| OS | カタログ既定位置 |
| --- | --- |
| Windows | `%LOCALAPPDATA%\\HakoniwaDesktopManager\\approved-core-catalog.json` |
| macOS | `~/Library/Application Support/HakoniwaDesktopManager/approved-core-catalog.json` |
| Linux | `~/.local/share/HakoniwaDesktopManager/approved-core-catalog.json` |

現時点でCore Proの公式GitHubリリースはソース配布を中心としており、アプリがそのまま取得できる全OS向け検証済み実行バイナリの一覧は確認できません。[6] そのため、アプリは**未検証バイナリを自動ダウンロードしません**。`config/approved-core-catalog.example.json`を参照し、`publish-core-artifacts.yml`で生成・検証・公開したアーカイブのURLとSHA-256をカタログへ登録してください。詳細は[承認カタログ運用ガイド](docs/APPROVED_CORE_CATALOG.md)を参照してください。

## ディレクトリ構成

```text
hakoniwa-desktop-manager/
├── src/                         # React + TypeScript UI
├── src-tauri/                   # Rustのローカル実行・監視層
├── config/                      # Coreカタログの雛形
├── tools/                       # 起動ランチャ、カタログ生成ツール
├── .github/workflows/           # OS別CoreアーティファクトCI
└── docs/                        # アーキテクチャ、運用、検証資料
```

## 既知の制約と次の拡張

初期版の静的インポーターは、Launcherの`assets[]`、Endpoint形式、Bridgeの代表的な`routes`／`bridges`／`mappings`配列を対象にしています。固有Recipeの独自フィールドは警告として残し、利用者が画面で補正できます。

Core内の共有メモリPDU通信を完全に自動集計するには、対象バージョンでCore Proのデータ受信イベントを用いる読み取り専用monitorアセットを提供する必要があります。Core ProにはPDUチャネルのデータ受信イベントが用意されています。[1] これはアセットの書込み経路を変更せずに行う後続マイルストーンとし、初期版ではEndpoint／Bridge観測を優先します。

## 参照

[1]: https://github.com/hakoniwalab/hakoniwa-core-pro "hakoniwa-core-pro README"
[2]: https://github.com/ykikuchii/hakoniwa-business-pack/blob/main/catalog/index.yaml "Hakoniwa Business Pack component catalog"
[3]: https://github.com/hakoniwalab/hakoniwa-pdu-endpoint "hakoniwa-pdu-endpoint README"
[4]: https://github.com/ykikuchii/hakoniwa-business-pack/blob/main/catalog/components/hakoniwa-pdu-bridge-core.yaml "PDU Bridge Core catalog"
[5]: https://github.com/hakoniwalab/hakoniwa-business-pack/blob/main/docs/hakoniwa-runtime-primer.md "Hakoniwa Runtime Primer"
[6]: https://github.com/hakoniwalab/hakoniwa-core-pro/releases "Hakoniwa Core Pro releases"
