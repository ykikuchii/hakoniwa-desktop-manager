# 承認済みHakoniwa Coreカタログ運用ガイド

## 目的

Hakoniwa Desktop Managerは、端末上でCore実行ファイルを起動できるため、配布物の信頼性をアプリ機能の一部として扱います。このガイドは、OS別Coreアーカイブを**誰が、どのソースから、どの手順で生成し、どのハッシュを承認したか**を明確にするための運用手順です。

> アプリは、カタログに含まれないURL、HTTP URL、64桁の16進SHA-256を持たないアーティファクト、またはハッシュ不一致のアーカイブを導入しません。

## カタログ形式

カタログはJSONであり、`schema_version: 1`、`component: "hakoniwa-core-pro"`を必須とします。各アーティファクトに、対象OS、CPU、HTTPS URL、SHA-256、アーカイブ形式、展開後の`hako-cmd`相対パス、来歴を記録します。

| フィールド | 必須 | 説明 |
| --- | --- | --- |
| `version` | はい | Core Proのタグ。例: `v1.3.0`。 |
| `source_revision` | はい | ビルド元の不変コミットSHA。タグだけを信頼せず、実際にcheckoutしたコミットを固定します。 |
| `platform` | はい | `windows`、`macos`、`linux`のいずれかです。 |
| `architecture` | はい | `x64`または`arm64`です。 |
| `url` | はい | HTTPSで配布される不変URLです。公開後に同じURLの内容を差し替えないでください。 |
| `sha256` | はい | 配布アーカイブ全体の64桁16進SHA-256です。 |
| `archive_format` | はい | 現在は`zip`または単一実行ファイルの`file`です。 |
| `hako_cmd_relative_path` | はい | アーカイブ展開後の`hako-cmd`への相対パスです。Windowsでは`bin/hako-cmd.exe`です。 |
| `provenance` | はい | 元リポジトリ、Coreタグ、実行したCIワークフロー名を記録します。 |

雛形は[`../config/approved-core-catalog.example.json`](../config/approved-core-catalog.example.json)を参照してください。プレースホルダーのURLやハッシュを実運用のカタログへコピーしてはいけません。

## 承認フロー

| 段階 | 実施者 | 実施内容 | 証跡 |
| --- | --- | --- | --- |
| 1. 固定 | リリース管理者 | `hakoniwalab/hakoniwa-core-pro`のリリースタグとコミットSHAを確定します。 | 承認チケット、タグ、SHA |
| 2. OS別ビルド | CI | Windows x64、macOS x64、macOS ARM64、Linux x64で同一コミットをビルドします。 | CI run URL、ビルドログ |
| 3. 機能確認 | CI／レビュー担当者 | `hako-cmd`の存在、Coreのdoctor、代表アセットの起動／停止、必要なライブラリの解決を確認します。 | テスト結果、対象環境 |
| 4. アーカイブ化 | CI | install prefixの内容をZIP化し、SHA-256を生成します。 | ZIP、`SHA256SUMS` |
| 5. 承認 | 二者レビュー | タグ、source revision、URL、ハッシュ、platform、`hako-cmd`パスを照合します。 | プルリクエスト承認 |
| 6. 公開 | リリース管理者 | 不変のリリースURLとカタログを公開します。 | リリースページ、catalog JSON |
| 7. 端末導入 | アプリ | HTTPS取得、SHA-256照合、ZIPパス検査、アトミック展開を行います。 | アプリの導入日時・選択Core情報 |

## CIの利用

`.github/workflows/publish-core-artifacts.yml`は、Core Proの指定コミットを4つのOS／CPU構成でビルドし、ZIP、`SHA256SUMS`、`approved-core-catalog.json`をアプリ管理リポジトリのリリースへ添付します。

公開前に、ワークフローの以下を組織の実際の環境に合わせてレビューしてください。

1. Core Proのビルド・インストール手順はリリース対象タグに存在すること。
2. macOSの署名・公証要件、およびWindowsのコード署名要件を組織の配布方針に従って追加すること。
3. Python 3.12、CMake、C/C++コンパイラ、GTestなど、Core Proの`doctor`が要求する前提条件を各runnerで満たすこと。
4. 生成した`hako-cmd`、ライブラリ、設定、offsetファイルの配置が`hako_cmd_relative_path`と一致すること。
5. GitHub releaseの同名tagへ既存アセットを上書きしないこと。

Core ProはWindows、macOS、Linuxをサポートし、OS固有ビルド処理を`tools/hako.py`から委譲する設計です。[1] ただし、CIでのビルド成功はアプリ経由の全Recipeの実行成功を保証しません。代表Recipeごとの起動・通信試験は別途必要です。

## 手動でカタログを生成する場合

各OSでビルド済みのZIPを集めた後、次のようにカタログを作成します。

```bash
python3 tools/create_core_catalog.py \
  --version v1.3.0 \
  --revision 841a8bad447e6b5d549b4f4d543346e1817e37e8 \
  --release-base-url https://github.com/<owner>/<repo>/releases/download/v1.3.0 \
  --artifact linux-x64=dist/hakoniwa-core-pro-v1.3.0-linux-x64.zip \
  --artifact macos-x64=dist/hakoniwa-core-pro-v1.3.0-macos-x64.zip \
  --artifact macos-arm64=dist/hakoniwa-core-pro-v1.3.0-macos-arm64.zip \
  --artifact windows-x64=dist/hakoniwa-core-pro-v1.3.0-windows-x64.zip \
  --output dist/approved-core-catalog.json
```

生成直後に、JSONをレビューし、各アーカイブについて`sha256sum`またはOS標準のハッシュコマンドで値を再照合してください。承認後にカタログをアプリの既定位置へ配置します。

## ロールバック

Coreの導入先はバージョンごとに分離されます。新規バージョンで問題が出た場合は、ワークスペースの`core_release`を直前の検証済みバージョンへ戻してください。実行中のCoreコントローラーとアセットを先に正常終了させ、共有メモリの残存状態がないことを確認してから切り替えます。

アーカイブ、ハッシュ、来歴のいずれかに不整合が見つかった場合は、当該リリースをカタログから即時削除し、既に導入済みの端末へ再導入停止を通知してください。

## 参照

[1]: https://github.com/hakoniwalab/hakoniwa-core-pro "hakoniwa-core-pro README"
