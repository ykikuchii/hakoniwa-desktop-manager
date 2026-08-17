# 検証記録

## 概要

この記録は、Hakoniwa Desktop Manager v0.1.0の実装時点における検証結果を示します。Linux x64では実際にTypeScript、Rust、ユニットテスト、ネイティブバンドル生成を実行しました。WindowsおよびmacOSについては、対応コードとOS別ビルドCIを用意していますが、このLinuxビルド環境上で動作実機検証はしていません。したがって、**実装済み**、**Linuxで検証済み**、**CIで検証予定**を区別します。

| 項目 | 状態 | 根拠 |
| --- | --- | --- |
| React/TypeScriptの型検査 | **Linuxで検証済み** | `pnpm run check`が成功。 |
| フロントエンド本番ビルド | **Linuxで検証済み** | `pnpm run build`が成功。 |
| Rustコンパイル | **Linuxで検証済み** | `cargo check --manifest-path src-tauri/Cargo.toml`が成功。 |
| Rustユニットテスト | **Linuxで検証済み** | 26件が成功。内訳は下表。 |
| Linuxネイティブバンドル | **Linuxで検証済み** | DEB、RPM、AppImageの3形式を生成。 |
| Windows x64実行 | **CIで検証予定** | TauriのWindowsバンドルおよびCoreビルドCIを用意。実Windows環境で未実行。 |
| macOS x64/ARM64実行 | **CIで検証予定** | TauriのmacOSバンドルおよびCoreビルドCIを用意。実macOS環境で未実行。 |
| Windows→WSL2起動 | **実装済み・実機未検証** | `wsl.exe`ランナーを実装。実Windows/WSL2環境での統合試験が必要。 |
| 実Core Pro／実PDU通信 | **代表Recipeで未検証** | Coreアーカイブの承認カタログと対象Recipeが未提供のため、実接続試験は次段階。 |

## 実行したコマンド

```bash
pnpm install
pnpm run check
pnpm run build
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
pnpm tauri build
```

Rustユニットテストで確認した対象は次のとおりです。v0.1.0時点の7件に、その後の不具合修正にともなう回帰テスト19件を加えた26件です。

| テスト | 確認事項 |
| --- | --- |
| `core::rejects_non_https_artifact` | HTTPのCore配布URLを拒否する。 |
| `core::rejects_parent_directory` | ZIP展開時の`..`パスを危険として扱う。 |
| `commands::starts_dependencies_first` | アセットの依存順序を解決する。 |
| `commands::monitor_targets_follow_resolved_owner` | ログの帰属先を解決済みの`owner_asset_id`で決める。 |
| `commands::monitor_targets_ignore_config_file_overlap` | 成立しなかった旧条件（`endpoint_config`と`config_files`の一致）を復活させない。 |
| `commands::monitor_targets_keep_bridge_name_fallback` | 解決結果を持たない旧`workspace.json`でもBridge名で帰属できる。 |
| `importer::classifies_transports` | WebSocketとSHMをtransportとして正規化する。 |
| `importer::parses_launcher_asset` | Launcherの`assets[]`と`after_start`を読み取る。 |
| `importer::parse_endpoint_leaves_links_unset` | 解析層はアセットとの突き合わせを行わない。 |
| `linking::links_endpoints_to_assets_by_name` | 接続の端点をアセット名で解決する。 |
| `linking::links_endpoints_despite_whitespace_and_case` | 前後空白・区切り文字・大小の違いを吸収する。 |
| `linking::links_owner_by_config_path_in_command_args` | 名前が一致しなくても、設定ファイルのパスで所有アセットへ紐づける。 |
| `linking::links_endpoint_named_after_its_config_file` | 設定ファイル名から採られたエンドポイント名を解決する。 |
| `linking::links_owner_by_bridge_name` | Bridge由来の接続を端点ではなくBridgeアセットへ帰属させる。 |
| `linking::keeps_non_asset_endpoints_unlinked_and_silent` | 外部ホストや内部キャッシュを未解決のまま扱い、警告を出さない。 |
| `linking::warns_for_each_unresolved_endpoint` | 紐づかない端点を、送信元／宛先を名指しで警告する。 |
| `linking::refuses_ambiguous_config_path_match` | 候補が複数ある設定パスでは先勝ちせず未解決にする。 |
| `linking::re_resolution_follows_asset_changes` | アセットの追加・削除に解決結果が追従する。 |
| `monitor::records_message_as_connected` | PDU送受信のログをConnected状態へ変換する。 |
| `process::rejects_empty_program` | 空の実行プログラムを拒否する。 |
| `process::start_returns_without_deadlock` | `start`がロックを保持したまま`snapshot`を呼ばない。 |
| `process::requested_stop_is_not_reported_as_failure` | 利用者操作による停止をシグナル終了でも異常終了として記録しない。 |
| `process::stop_snapshot_keeps_earlier_output` | 停止後のスナップショットが既存のログ末尾を保持する。 |
| `process::stopping_twice_is_idempotent` | 停止済みプロセスへの再停止で状態を壊さない。 |
| `process::caps_line_length_without_newline` | 改行を出さない出力でも1行の保持量を上限で抑える。 |
| `process::stop_owner_reports_nothing_for_unknown_owner` | 未知のownerに対する停止要求を空の結果として返す。 |

回帰テストは、対応する修正を1つずつ元に戻した版でそれぞれが失敗することを確認したうえで採用しています。テストが緑であることと、欠陥を検出できることは別であるため、新しい回帰テストを追加する際は同じ確認を行ってください。

### 測定環境の補足

上表の26件は、Windowsホスト上のWSL Ubuntu 24.04（rustupで導入したstableツールチェーン、Tauriの依存はaptで導入）で`cargo test`を実行して確認しました。v0.1.0当初の7件はLinux x64のビルド環境で確認したものです。フロントエンドにはテストランナーを導入していないため、`src/selectors.ts`などのTypeScript側の純関数は型検査とビルドのみで、単体テストはありません。

## Linuxで生成した配布物

| 形式 | 生成先 | 利用対象 |
| --- | --- | --- |
| DEB | `src-tauri/target/release/bundle/deb/Hakoniwa Desktop Manager_0.1.0_amd64.deb` | Debian／Ubuntu系Linux |
| RPM | `src-tauri/target/release/bundle/rpm/Hakoniwa Desktop Manager-0.1.0-1.x86_64.rpm` | Fedora／RHEL系Linux |
| AppImage | `src-tauri/target/release/bundle/appimage/Hakoniwa Desktop Manager_0.1.0_amd64.AppImage` | 汎用Linux x64 |

### Linuxでの手動受入試験

Linuxでインストール後、次の順に確認してください。

1. アプリを起動し、ワークスペース画面が表示されることを確認します。
2. Core管理画面で、承認カタログが空である場合には未検証Coreの導入を促さず、管理者のカタログ登録が必要と表示されることを確認します。
3. Recipeまたは設定ディレクトリを選択し、Launcherのasset、Endpoint、Bridgeがプレビューに現れることを確認します。
4. 個別アセットを追加し、コマンドと引数を分けて保存、起動、停止できることを確認します。
5. Bridgeまたはmonitorアセットの出力に`connect`、`send pdu=<name> bytes=<n>`、`disconnect`を出力し、接続画面で状態、最終活動、PDU、送受信カウンタが更新されることを確認します。
6. `hako-cmd start`を実行できるCoreを選択した場合、Coreコントローラーの実行状態とは別に時刻制御結果を確認します。

## OS別の追加受入試験

### Windows x64 と WSL2

| 観点 | 受入条件 |
| --- | --- |
| Windowsネイティブ | `hako-cmd.exe`の検証済み配布物を導入し、アセットの起動・停止・ログ取得ができる。 |
| WSL2ディストリビューション | `Ubuntu`など明示したディストリビューションで、`cwd`、環境変数、プログラム、引数が期待どおり実行される。 |
| 終了 | WSL内部の親プロセスおよび子プロセスがRecipeの正常終了手順で停止する。強制停止のみへ依存しない。 |
| パス | WindowsパスとWSLパスを混在させず、WSL設定にはLinux形式の`cwd`を使う。 |

### macOS x64／Apple Silicon

| 観点 | 受入条件 |
| --- | --- |
| CPU別配布物 | x64とarm64のカタログエントリが分離され、誤ったアーキテクチャを選択しない。 |
| セキュリティ | アプリとCoreアーカイブに組織の署名・公証方針を適用する。 |
| ダイナミックライブラリ | Coreに必要なライブラリ検索パスと`hako-cmd`の実行が両CPUで正常である。 |

## 実Core通信の受入シナリオ

最初の正式な受入対象は、Core ProのPDU communicationサンプル、またはBusiness Packの代表Recipe一つとします。Core Proはアセット登録、PDUチャネル作成、`hako-cmd start`後のread/writeというライフサイクルを用います。[1]

| 手順 | 期待結果 |
| --- | --- |
| Core Proを承認済みカタログから導入 | SHA-256一致、`hako-cmd`検出、選択Coreとして保存。 |
| 代表Recipeを取り込む | アセット、PDU設定、Endpoint／Bridge経路がプレビューされる。 |
| Coreコントローラーと二つのアセットを起動 | PID、ログ、起動順序がダッシュボードに反映される。 |
| `hako-cmd start`を実行 | シミュレーション時間とPDU更新が開始される。 |
| Bridge monitorを接続 | 経路がConnectedとなり、最終活動、PDU名、送受信数が表示される。 |
| 片方のアセットまたは経路を停止 | DisconnectedまたはFailedが詳細画面に表示され、最後のログを確認できる。 |
| 正常終了 | `hako-cmd stop`の後、Launcher／アセットをRecipeの正常終了手順で停止する。 |

## 残る検証作業

1. Windows x64、Windows+WSL2、macOS x64、macOS ARM64のCIおよび実機で、アプリのインストーラーとCoreアーカイブを検証する。
2. `publish-core-artifacts.yml`を一度実行し、実ハッシュを持つカタログを生成・二者承認する。
3. Business Packの代表Recipe一つを選び、Launcher、Endpoint、Bridgeの実ファイルを使った受入シナリオを実行する。
4. Core Proのread-only PDU monitor adapterを追加し、共有メモリのみのPDU通信についてもイベント時刻・方向・件数を記録する。
5. フロントエンドのテストランナーを導入するか判断する。現状`src/selectors.ts`のプロセス選択・接続の対応付けは純関数だが単体テストがなく、回帰は目視受入に依存している。

## 参照

[1]: https://github.com/hakoniwalab/hakoniwa-core-pro "hakoniwa-core-pro README"

## 生成済みLinux配布物の整合性情報

以下はこのLinux x64検証環境で生成したv0.1.0のハッシュです。再ビルド、署名、依存ライブラリの更新により値は変わるため、公開時はリリースCIで改めて算出してください。

| 配布物 | サイズ | SHA-256 |
| --- | ---: | --- |
| `Hakoniwa Desktop Manager_0.1.0_amd64.deb` | 4,748 KB | `393858db7254a48077ca2a0d1afda35577fd67449430ae8d0214fd75b8343434` |
| `Hakoniwa Desktop Manager-0.1.0-1.x86_64.rpm` | 4,748 KB | `93ec48707be99b1c46712f1e72dcd1395885c77e9007ca4cebcef73b8e093c70` |
| `Hakoniwa Desktop Manager_0.1.0_amd64.AppImage` | 77,376 KB | `ecda95db796e5f1f608a055fc6cf903d4db819f7c6078c5d16e194da1d91422d` |
