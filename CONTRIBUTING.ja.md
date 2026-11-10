[English](CONTRIBUTING.md) | 日本語

# PolyRec への貢献

PolyRec への貢献に関心をお寄せいただき、ありがとうございます！このガイドでは、貢献を始めるための手順を紹介します。

## クイックスタート

### 1. フォークとクローン

```powershell
git clone https://github.com/yusukensanta/polyrec.git
cd polyrec
```

### 2. 前提条件

- Windows 10 2004 以降（ビルド 19041 以降）。Process Loopback Capture API を使うために必要です。
- Direct3D 11 に対応した GPU。
- [rustup](https://rustup.rs/) でインストールした Rust。`rust-toolchain.toml` でバージョン（1.99.1）が固定されており、最初のビルド時に rustup がそのバージョンをインストールします。環境変数 `RUSTUP_TOOLCHAIN` を設定すると、この固定より優先されます。その値が古いバージョンを指している場合、Cargo は「requires rustc 1.99」と表示して停止します。

### 3. ビルドとテスト

```powershell
cargo build              # デバッグビルド
cargo build --release    # リリースビルド（target/release/polyrec.exe）
cargo test               # テストスイートを実行
cargo clippy --all-targets
```

**注:** いくつかのテストは、モックではなく実際の Windows API（ウィンドウの列挙、D3D11 デバイスの作成、WASAPI キャプチャ）を使って動作を検証します。そのため、ディスプレイのないヘッドレスな CI ではなく、実際の Windows デスクトップセッション上で実行する必要があります。リリース CI では、これらのテストを GitHub ホストの `windows-latest` ランナーで実行しています。

## 開発ワークフロー

### 変更の手順

1. **ブランチを作成**
   ```powershell
   git switch -c feat/my-feature
   ```

2. **変更を加える**
   - コードを書きます。
   - 変更がテスト可能な場合はテストを追加します（純粋なロジックや設定の解析など。ここではすべてがテスト可能なわけではありません。下記の「テスト」を参照してください）。
   - `CHANGELOG.md` の `[Unreleased]` の下を更新します。
   - ドキュメントにはそれぞれ日本語版（`README.ja.md`、`docs/USER_GUIDE.ja.md`、`CHANGELOG.ja.md` など）があります。両方に同じ変更を加えてください。

3. **確認**
   ```powershell
   cargo build --release
   cargo clippy --all-targets
   cargo test
   ```

4. **変更をコミット**
   ```powershell
   git add .
   git commit -m "feat: add awesome feature"
   ```

   [Conventional Commits](https://www.conventionalcommits.org/) に従っています。
   - `feat:` — 新機能
   - `fix:` — バグ修正
   - `perf:` — パフォーマンスの改善
   - `docs:` — ドキュメントの変更
   - `test:` — テストの追加・修正
   - `refactor:` — 動作を変えないコードの変更
   - `ci:` — CI/CD パイプラインの変更
   - `chore:` — メンテナンス作業（バージョンの引き上げ、依存関係の更新）

5. **プッシュしてプルリクエストを作成**
   ```powershell
   git push origin feat/my-feature
   ```

### アプリケーションをローカルで実行する

```powershell
cargo run
```

## コード品質

### リント

```powershell
cargo clippy --all-targets
```

CI では、新たに出た clippy の警告はマージ前に修正すべきものとして扱います。現在のコードベースは clippy の警告なしでビルドできます。

### テスト

```powershell
cargo test               # 高速なテスト
cargo test -- --ignored  # 実際のデスクトップ/ハードウェアが必要なテスト（D3D11、ライブキャプチャ）も実行
# ignored のテストは実際のウィンドウとオーディオデバイスをキャプチャし（何も再生はしません）、
# 数分かかります。名前を指定して1つだけ実行するには
# `-- --ignored --exact session::tests::full_capture_produces_nonempty_file` を使います。
```

## プロジェクト構成

```
polyrec/
├── src/
│   ├── main.rs                  # エントリポイント、Media Foundation のライフタイム、モジュール宣言
│   ├── config.rs                # 設定の構造体 + 型付きの設定用 enum、TOML の読み込み/保存
│   ├── i18n.rs                  # 表示言語の文字列テーブル（EN/JA）
│   ├── error.rs                 # AppError（thiserror）+ ResultExt（`.encode_ctx(..)` など）
│   ├── types.rs                 # 共有の型: CaptureSource、AudioDevice、VideoFrame、...
│   ├── sources.rs               # ウィンドウ/モニター/プロセスの列挙、exe 名/アイコン
│   ├── disk_space.rs            # ディスク空き容量のチェック + 定期的な監視
│   ├── recording_naming.rs      # 完成したファイルの命名（プロセス名 / カスタム接頭辞 + 連番）
│   ├── update_check.rs          # GitHub Releases によるバージョン確認
│   ├── self_update.rs           # 検証付きダウンロード + インプレース更新 / インストーラーの起動
│   ├── hotkeys/mod.rs           # グローバルショートカット（RegisterHotKey + 低レベルキーボードフック）
│   ├── capture/
│   │   ├── mod.rs               # CaptureControl: 共有クロック + 一時停止/停止シグナル
│   │   ├── video.rs             # Windows.Graphics.Capture のパイプライン
│   │   ├── audio.rs             # WASAPI のデバイス/ループバック + プロセス単位のループバックキャプチャ
│   │   └── device.rs            # D3D11 デバイスの作成
│   ├── encode/
│   │   ├── mf.rs                # MfSession: RAII による MFStartup/MFShutdown
│   │   ├── writer.rs            # IMFSinkWriter のセットアップ（EncoderParams）、メディアタイプ
│   │   ├── actor.rs             # 録画アクター + キャプチャ->エンコーダーのポンプタスク
│   │   ├── remux.rs             # 録画後のトラック選択 / ミックスによるエクスポート
│   │   └── highlight_export.rs  # ハイライトのセグメントを連結 + トリムして1つのファイルにする
│   ├── highlight/mod.rs         # ハイライトのセグメントをローテーションするローリングアクター
│   ├── session/
│   │   ├── mod.rs               # SessionManager: 録画 + ハイライトのライフサイクル
│   │   ├── pipeline.rs          # CapturePipeline: 映像 + 音声のキャプチャスレッドを起動/停止
│   │   ├── state.rs             # Idle/Recording/Paused の状態マシン
│   │   └── clock.rs             # 一時停止を考慮した録画クロック（QPC ベース）
│   └── ui/dashboard/            # egui ダッシュボード
│       ├── mod.rs               # App 構造体 + eframe のエントリポイント
│       ├── state.rs             # 機能ごとの App サブステート（エクスポート、アプリ追加ピッカー、...）
│       ├── actions.rs           # UI の背後で動く開始/停止/リフレッシュのアクション
│       ├── background.rs        # バックグラウンドジョブのフレームごとのポーリング
│       ├── panel_*.rs           # メニューバー、ソース一覧、中央パネル
│       ├── *_popup.rs, popups.rs# 画質 / オーディオ / ショートカットのポップアップ、エラー/更新ダイアログ
│       ├── overlay.rs           # 録画 HUD オーバーレイのビューポート
│       └── theme.rs, widgets.rs, util.rs
├── assets/                      # アプリアイコン
├── installer/                   # Inno Setup スクリプト
├── .github/workflows/           # CI + リリースパイプライン
├── Cargo.toml
└── build.rs                     # アプリアイコンを exe に埋め込む
```

## よくある作業

### 設定項目を追加する

1. `config.rs` 内の該当する構造体にフィールドを追加し、その構造体の `Default` 実装でデフォルト値を設定します。すべての設定構造体は `#[serde(default)]` なので、そのフィールドがない古い `config.toml` も読み込めます。フィールドごとの serde 属性は不要です。
2. 選択肢が決まっている場合は、`String` ではなく enum を使い、`#[serde(rename_all = "lowercase")]` と `#[serde(other)]` のフォールバック用バリアント（最後のバリアントにする必要があります）を付けます。`Codec` / `EncoderMode` を参照してください。
3. 既存の設定テストと並べて、ラウンドトリップテストを追加します。
4. ユーザーに見える設定であれば、UI（`src/ui/dashboard/`）に組み込みます。

### UI テキストを追加する

静的な UI 文字列はすべて `src/i18n.rs` の `Strings` 構造体にあります。`src/ui/dashboard/` 以下に文字列リテラルをハードコードするのではなく、そこにフィールドを追加してください（`EN` と `JA` の両方のインスタンスに追加します。どちらかでフィールドが欠けているとコンパイルエラーになります）。

### デバッグ

```powershell
$env:RUST_LOG="debug"; cargo run
```

構造化ログには `tracing`/`tracing-subscriber` を使用しています。

## CI/CD

リリースパイプライン（`.github/workflows/release.yml`）は `v*.*.*` タグのプッシュで起動し、次の処理を行います。
- タグが `Cargo.toml` の `version` フィールドと一致することを検証します（両者がずれた場合は、`Cargo.toml` のバージョンを上げてタグを付け直してください）。
- `cargo build --release --locked` でビルドします。
- zip にパッケージ化し、`SHA256SUMS.txt` を生成して、ビルドの来歴（build provenance）の構成証明を作成します。
- **ドラフト**の GitHub Release として残します。メンテナーがそれに署名して公開します（下記の「リリース手順」を参照してください）。

PR を提出する前に、`cargo build --release`、`cargo clippy --all-targets`、`cargo test` がすべて通ることを確認してください。

## ヘルプ

- 📖 **ドキュメント**: [docs/USER_GUIDE.ja.md](docs/USER_GUIDE.ja.md) で、使い方、機能、動作要件を説明しています。
- 🐛 **Issue**: 既存の Issue を確認するか、新しい Issue を作成してください。

## プルリクエストのガイドライン

### 提出前のチェック

- [ ] `cargo build --release` が成功
- [ ] `cargo test` が合格
- [ ] `cargo clippy --all-targets` で警告なし
- [ ] コミットメッセージが Conventional Commits に準拠
- [ ] `CHANGELOG.md` の `[Unreleased]` の下を更新済み（ユーザーに見える変更の場合）
- [ ] ドキュメントと `CHANGELOG.md` の変更を、対応する `.ja.md` にも反映済み
- [ ] PR の説明で、何を変更したか、なぜ変更したかを説明済み

## リリース手順

（メンテナー専用）

1. `CHANGELOG.md` と `CHANGELOG.ja.md` を更新します。`[Unreleased]` の項目を新しいバージョンの見出しの下に移動します。
2. それに合わせて `Cargo.toml` の `version` フィールドを上げます。
3. コミットしてプッシュしたあと、タグを付けてそのタグをプッシュします。
   ```powershell
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```
4. CI がビルドし、タグが `Cargo.toml` と一致することを検証して、ドラフトリリースを残します。`.github/workflows/release.yml` を参照してください。
5. 署名して公開します。タグをプッシュしたリポジトリのチェックアウトで、自分で開いたターミナルから実行してください（minisign がリリース鍵のパスワードを尋ねます）。
   ```powershell
   pwsh -NoProfile -File $env:USERPROFILE\.minisign\Sign-Release.ps1 vX.Y.Z
   ```
   これは `.github/scripts/Sign-Release.ps1` のコピーで、リポジトリの外、鍵と同じ場所に置いてあります。GitHub にプッシュできる人なら誰でもリポジトリ内のコピーを変更できるうえ、このスクリプトはあなたがパスワードを入力している間に実行されるためです。内容を読んでから一度インストールしてください（`Copy-Item .github/scripts/Sign-Release.ps1 $env:USERPROFILE\.minisign\`）。リポジトリ内のコピーが変更されたとスクリプトから知らされたら、差分を読んだうえで再インストールしてください。

   このスクリプトは次のことを行います。
   - そのタグ時点の `keys/` があなたの公開鍵であることを確認します。
   - あなたが前回署名したリリース以降の変更内容を一覧表示し、ビルド、リリース、鍵に影響するファイルにはフラグを付けて、あなたがタグを入力するのを待ちます。
   - ドラフトのファイルを確認します。チェックサムと、`main` 上のそのタグのコミットからのビルドの来歴です。
   - `SHA256SUMS.txt` に署名します。
   - `SHA256SUMS.txt.minisig` をアップロードします。
   - ドラフトに含まれるファイルがちょうどその4つだけであることを確認してから、公開します。

   PolyRec のセルフアップデーターは、`keys/` 内のいずれかの鍵で署名されたリリースだけをインストールします。対象となるのは、リリース鍵と、それを紛失した場合に備えてオフラインで保管しているバックアップ鍵（`-KeyFile <backup key>`）です。

   署名することは、あなたがリリースの中身に目を通したと表明することです。ですので、一覧は必ず読んでください。入力の手間を省くために、リリース鍵からパスワードを外すことは決してしないでください。

## 行動規範

- 敬意を持ち、誰でも参加しやすい態度で接してください。
- 建設的なフィードバックをしてください。
- 人ではなく、コードに焦点を当ててください。

## ライセンス

貢献することにより、あなたは自分の貢献が Apache 2.0 License の下でライセンスされることに同意するものとします。

---

**質問がありますか？** Issue を作成してください。
