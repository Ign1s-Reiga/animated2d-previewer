# Animated2D Desktop Viewer

ゲームから抽出した 2D キャラクターアセットを、**元ゲームのランタイムに依存せず**再生する
デスクトップビューア／ランタイムです。目的はひとつ、**デスクトップでキャラクターを正しく
再生すること**です。

対象のソースエコシステム:

- **Live2D Cubism**（Cubism 3 以降。Cubism 2 は未対応）
- **Spine**（複数の歴史的バージョン）
- 上記を Unity の AssetBundle に同梱した変種

インポータは対象タイトル名ではなく、復元するアセットの形で命名しています。

| インポータ | 入力 |
| --- | --- |
| `unity_cubism` | Unity AssetBundle に同梱された Cubism モデル |
| `spine_bytes` | `.skel.bytes` / `.atlas.txt` / `.png` で配布される Spine スケルトン |
| `unity_spine` | Unity AssetBundle に同梱された Spine リグ（立ち絵・idle のみ） |
| `generic` | 素の Spine JSON / バイナリ / atlas |

---

## 現在の状態

Spine と Cubism の両方をインポートし、同じ透過デスクトップビューアで再生できます。

- **Spine:** 3.8 バイナリは、Unity に同梱された実物のリグで検証済みです。IK /
  transform / path の各コンストレイント、deform、描画順、クリッピング、ミキシング、
  bounding box によるヒット判定を実装しています。
- **Cubism:** MOC3 を独自に解析し（Cubism Core は使いません）、AnimationClip から復元した
  モーション、物理演算、視線追従リグ、アニメーションする描画順、グルー、デフォーマの
  不透明度を再生します。エモーションは元の AnimatorController の遷移どおり 1 回再生して
  既定モーションへ戻ります。実在する 3 体のモデル（各フル版と軽量版）で確認しています。

**未検証の点:**

- Spine 4.0 / 4.1 バイナリデコーダは実物のエクスポートで検証していません。テストは
  自前のフィクスチャライタとの往復のみで、デコーダとライタが互いに一致していることしか
  示しません。そのためカウント値・文字列参照・ファイル終端を厳しく検証し、レイアウトが
  違えば壊れた結果を返さず明確なエラーで落ちます。4.x は JSON 形式の利用を推奨します。
- Spine の transform / path コンストレイントは、プロパティテストと手計算の幾何で
  固めたもので、公式ランタイムとの比較はしていません。複数キャラクターで idle が正しく
  再生されることの目視確認も完了していません。
- Cubism の評価規則は手計算のテストと実モデルでの確認で固めたもので、公式ランタイムとの
  比較はしていません。根拠と未確定事項は [`docs/cubism-moc3.md`](docs/cubism-moc3.md) に
  記録しています。

**既知の未対応:**

- Spine: 2.x / 4.2 バイナリ。4.0 / 4.1 バイナリは読み込むたびに未検証である旨を報告します。
- Cubism: 表情、ポーズ、パーツ不透明度カーブ、モデル単位のカーブ（不透明度・まばたき・
  リップシンク）、複数テクスチャページ、ヒット判定、作者が設定したモーションのフェード
  時間（クロスフェードはビューアの既定時間を使います。AnimatorController の遷移で既定
  モーションへ戻る場合だけは元の時間を使います）。

いずれも黙って捨てず、読み込み時に報告します。

---

## 設計の核

**Spine と Cubism を相互変換しません。** 双方を別々の内部 IR に正規化し、共通の
インターフェース（`AnimatedModel`）の裏に隠します。モデルはレンダラ非依存の描画
プリミティブ（`RenderMesh`）を出力し、レンダラはそれを描くだけです。

```text
Game Asset Bundle / Extracted Files
        ↓  importers/   アセットの探索と再構成のみ
Source Format Detector
        ↓  formats/     バージョン別デコード → IR
Spine Decoder | Cubism Decoder
        ↓
Normalized Animated2D Model (IR)
        ↓  runtime/     決定論的なアニメーション評価
Runtime / Animator
        ↓  renderer/    ソース形式に非依存
Renderer
        ↓  desktop/
Desktop Host
```

守る不変条件は 2 つです。

1. **ゲーム固有の知識を `importers/` より下流に漏らさない**
2. **ソースバージョン固有の知識を `formats/` より下流に漏らさない**

---

## クレート構成

Rust の cargo workspace です。レンダリングは `wgpu`、デスクトップシェルは `winit` +
`tray-icon` を使います。

| クレート | 役割 |
| --- | --- |
| `a2d-core` | IR 型、数学、`AnimatedModel`、`RenderMesh`、エラー分類、`LoadReport` |
| `a2d-spine` | atlas パーサ、内容ベースのバージョン検出、JSON（2.x / 3.x / 4.x）とバイナリ（3.x / 4.0・4.1）のデコーダ |
| `a2d-cubism` | MOC3 の解析、キーフォーム補間、ワープ／回転デフォーマ、グルー、モーション、物理演算、視線追従 |
| `a2d-unity` | UnityFS コンテナ、serialized file、オブジェクト目録、MonoBehaviour 本体、AnimationClip、AnimatorController、Transform 階層の読み取り |
| `a2d-import` | `generic` / `spine_bytes` / `unity_cubism` / `unity_spine` の各インポータ |
| `a2d-pack` | `.a2dpack` の読み書き、マニフェスト、決定論的シリアライズ、`validate` |
| `a2d-runtime` | Spine のボーン変換、タイムライン、スキニング、コンストレイント、ミキシング、idle |
| `a2d-render` | wgpu 描画、テクスチャキャッシュ、バッチング、ブレンドモード、ステンシルクリッピング、オフスクリーン描画 |
| `a2d-desktop` | 透過フレームレスウィンドウ、ドラッグ、クリックスルー、最前面、トレイ、設定の永続化 |
| `a2d-cli` | `animated2d` バイナリ |

依存は一方向です。`a2d-render` と `a2d-runtime` は `a2d-import` や `a2d-unity` に
依存できません。逆向きに必要になった型は `a2d-core` へ移します。

---

## CLI

```bash
animated2d inspect  <input>                  # 形式・バージョン・テクスチャ・アニメーション・未対応機能を表示
animated2d import   <input> -o <out.a2dpack> # 正規化済みパッケージを作る
animated2d validate <package.a2dpack>        # パッケージを検査する
animated2d preview  <package.a2dpack>        # デスクトップビューアで開く
animated2d preview  <package.a2dpack> -o <dir>  # 0.0 / 0.25 / 0.5 / 1.0 秒を PNG に描画
```

- `<input>` は単一のアセットでもディレクトリでも構いません。判定はファイルの内容で行い、
  拡張子には頼りません。判定が曖昧なときは推測せずエラーにします。
- `--game <name>` で使うインポータを指定できます（`generic` / `spine_bytes` /
  `unity_cubism` / `unity_spine`）。`inspect` と `import` で使え、`preview` では
  Unity バンドルを直接開くときだけ効きます。
- `preview` に Unity バンドルを直接渡すと、パッケージを作らずにその場で開きます。
- `preview -o` は GPU が必要です。各フレームのドローコール数・三角形数・フレーム
  バッファのフィンガープリントを表示し、PNG を書き出します。
- `preview` の `--exit-after <秒>` は手動終了と同じ経路でビューアを閉じます。計測は
  ウィンドウが開いてから始まり、終了時に描画したフレーム数を表示します。`--hidden` は
  ウィンドウを不可視で開きます。どちらもスクリプトやテスト向けです。

### `validate`

テクスチャ欠損、未解決のアタッチメントや参照、非対応のタイムライン・コンストレイント、
不正なボーン親子やスロット参照、壊れた atlas 参照を検査します。インポート時に記録された
内容もあわせて表示します。

インポート時の記録は 2 種類に分かれます。

- **情報:** 元のランタイムとまったく同じに再生されることを確認できたもの（何も捨てず、
  近似もしていない）。表示はしますが、検査は失敗しません。
- **問題:** それ以外のすべて。データを捨てた、近似した、あるいは元と同じだと確認できて
  いないもの。1 件でもあれば終了コード 1 になります（パッケージを読み込めない場合は
  別の終了コードです）。

「情報」に分類するのは、元の挙動を検証できた場合だけです。

---

## ビューア

| 操作 | 動作 |
| --- | --- |
| ドラッグ | キャラクターを移動 |
| スクロール | 拡大縮小 |
| `Space` | 一時停止／再開 |
| `Tab` | 次のアニメーション |
| `M` | 次のモデル |
| `L` | マウスカーソルを目で追う（視線追従リグを持つ Cubism モデル） |
| `T` | 最前面表示の切り替え |
| `C` | クリックスルーの切り替え |
| `F` | 左右反転 |
| `R` | 位置とサイズをリセット |
| `Esc` | 終了 |

トレイメニューからは、モデルとアニメーションの選択、一時停止、最前面、クリックスルー、
カーソル追従、位置とサイズのリセット、終了を操作できます。クリックスルー中はウィンドウが
マウスを受け取らないため、トレイが唯一の操作手段になります（左右反転と拡大縮小は
トレイにありません）。

終了時に保存し、次回起動時に復元するもの:

- ウィンドウの位置とサイズ、最前面・クリックスルー・カーソル追従の設定（すべてのモデル）
- 選択中のモデルとアニメーション、拡大率、位置、左右反転（Spine のパッケージのみ。
  Cubism のパッケージや直接開いた Unity バンドルは記憶しません）

保存先は、Windows では `%APPDATA%\animated2d\config.json`、
macOS では `~/Library/Application Support/animated2d/config.json`、それ以外では
`$XDG_CONFIG_HOME/animated2d/config.json` です。環境変数 `A2D_CONFIG_DIR` で上書き
できます。

Windows では透過ウィンドウを DirectComposition 経由で描画します。表示には `--release`
ビルドを推奨します（デバッグビルドでは CPU 側の評価が遅くなります）。

---

## 内部パッケージ形式 `.a2dpack`

ビューアは生のゲームアセットを読まず、必ず正規化済みパッケージを読みます。

```text
character.a2dpack/
├─ manifest.json
├─ model.bin        # 正規化済み IR（ゲーム側のオブジェクトグラフではない）
├─ textures/
├─ animations/
└─ metadata/
```

```json
{
  "formatVersion": 9,
  "modelType": "spine",
  "sourceGame": "spine_bytes",
  "sourceFormat": "spine-3.8",
  "displayName": "CharacterName",
  "defaultAnimation": "idle",
  "textures": [],
  "animations": []
}
```

シリアライズは決定論的です（フィールド順は固定、マップはソート、浮動小数の書式も固定）。
レイアウトを変えるたびに `formatVersion` を上げ、ローダは古い版も読みます。
インポート時の記録は、問題が `importWarnings`、情報が `importNotes` に入ります（空なら
省略。`validate` を参照）。

---

## ビルドと開発

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

CI はプルリクエストと `main` への push ごとに、Linux で fmt・clippy・テスト、Windows で
clippy を実行します。主なターゲットは **Windows** です。

### ナイトリービルド

`main` にコミットがあった日に毎晩ビルドしたバイナリを、`nightly` プレリリースとして公開して
います。同じタグに上書きされるため、以下のリンクは常に最新版を指します。

| プラットフォーム | ダウンロード |
| --- | --- |
| Windows (x86_64) | [`animated2d-x86_64-pc-windows-msvc.zip`](https://github.com/Ign1s-Reiga/animated2d-previewer/releases/download/nightly/animated2d-x86_64-pc-windows-msvc.zip) |
| Linux (x86_64) | [`animated2d-x86_64-unknown-linux-gnu.tar.gz`](https://github.com/Ign1s-Reiga/animated2d-previewer/releases/download/nightly/animated2d-x86_64-unknown-linux-gnu.tar.gz) |
| macOS (Apple silicon) | [`animated2d-aarch64-apple-darwin.tar.gz`](https://github.com/Ign1s-Reiga/animated2d-previewer/releases/download/nightly/animated2d-aarch64-apple-darwin.tar.gz) |

一覧とリリースノートは [Releases](https://github.com/Ign1s-Reiga/animated2d-previewer/releases/tag/nightly)、
ビルドログは [Actions](https://github.com/Ign1s-Reiga/animated2d-previewer/actions/workflows/nightly.yml)
にあります。チェックサムは同じリリースの `SHA256SUMS` で検証できます。

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

- **プレリリースです。** タグ付きの安定版ではなく、`main` の状態をそのまま反映します。
- **署名していません。** Windows SmartScreen と macOS Gatekeeper が警告を出します。
  macOS では実行前に `xattr -d com.apple.quarantine animated2d` が必要です。
- **公開前にフォーマット・lint・テストを通します。CI に GPU はありません。** GPU テストとウィンドウテストは自動でスキップされます。
  ナイトリーが緑でも、描画が正しいことまでは保証しません。

---

## テスト

| 種別 | 内容 |
| --- | --- |
| ユニット | バイナリパーサ、バージョン検出、ボーン変換、スキニング、補間、ベジェ、描画順、クリッピング、atlas、命名の正規化 |
| ゴールデン | `ソースアセット → インポータ → IR → 決定論的シリアライズ` をコミット済みフィクスチャと比較 |
| ビジュアル回帰 | `0.0s / 0.25s / 0.5s / 1.0s` で描画し、決定論性・時間による変化・描画範囲を検査（Spine・Cubism とも）。基準画像はコミットせず、`A2D_BASELINE_DIR` を設定したときだけ許容誤差付きで比較 |
| 実アセット | `#[ignore]` 付きで、環境変数で指定したローカルの実アセットに対して実行（[`tests/README.md`](tests/README.md)） |

抽出したゲームアセットと proprietary SDK バイナリはコミットしません。実アセットは
gitignore された `tests/fixtures/local/` などローカルに置き、リポジトリには合成または
手書きの最小モデルと IR スナップショットだけを置きます。

ローカルのキャラクターを 1 体ずつ `inspect` / `import` / `validate` にかけるには
`python scripts/test_assets.py`（既定は gitignore 済みの `Test Assets/`、`A2D_TEST_ASSETS` や
`--assets` で変更可。[`tests/README.md`](tests/README.md)）。

---

## やらないこと

- Spine Editor / Cubism Editor との互換
- モデルの編集・オーサリング
- proprietary なプロジェクト形式への書き戻し
- 戦闘固有の挙動、ゲームサーバのエミュレーション
- Spine と Cubism の相互変換
- 3D レンダリング

---

## 設計上の決定

- **Cubism Core は使いません。** MOC3 は `a2d-cubism` が独自に解析します。プロジェクトを
  再配布可能に保ち、proprietary バイナリに依存しないためです。
- **Unity の読み取りは自前実装です。** `a2d-unity` が UnityFS と serialized file を
  直接読みます。外部依存は LZ4 の展開（`lz4_flex`）だけです。
- **デスクトップシェルは `winit` + `wgpu` + `tray-icon` です。**

技術資料:
[`docs/requirements.md`](docs/requirements.md)（仕様）、
[`docs/cubism-moc3.md`](docs/cubism-moc3.md)、
[`docs/cubism-physics.md`](docs/cubism-physics.md)、
[`docs/unity-animator-controller.md`](docs/unity-animator-controller.md)。

---

## ライセンス

本リポジトリの**ソースコード**は、以下のいずれかのライセンスの下で提供されます。
利用者はどちらかを選択できます。

- Apache License, Version 2.0
  ([LICENSE-APACHE](LICENSE-APACHE) または <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License
  ([LICENSE-MIT](LICENSE-MIT) または <http://opensource.org/licenses/MIT>)

Rust エコシステムの慣例に従ったデュアルライセンスです。各クレートの `Cargo.toml` にも
`license = "MIT OR Apache-2.0"` として記載しています（`publish = false` のため
crates.io へは公開されません）。トップレベルの [LICENSE](LICENSE) は両者への案内であり、
実際の条文は上記 2 ファイルです。

このライセンスが適用されるのは本リポジトリのコードのみです。ゲームアセットおよび
proprietary SDK バイナリはリポジトリに含まれておらず、それらの権利は各権利者に帰属します。

### コントリビューション

明示的に別段の定めをしない限り、Apache-2.0 ライセンス第 5 条の定義に従い、
本プロジェクトへ意図的に提出されたコントリビューションは上記のデュアルライセンスの下で
ライセンスされ、追加の条件は付されません。

---

## 免責

本プロジェクトは、**学術的用途および個人的用途**を想定して提供されます。

- **学術的用途** — 非公開フォーマットの解析と、その記録。
- **個人的用途** — 利用者自身が所有するソフトウェアから自分で抽出したアセットを、
  個人的にオフラインで閲覧すること。

DRM の回避、ライセンスチェックの迂回、ゲームサーバとの通信、アカウント操作の
自動化は実装しません。ゲームアセットおよび proprietary SDK バイナリの再配布も
行いません。抽出したアセットの第三者への配布も想定していません。

アセットの取得・保持・利用が各権利者の利用規約および適用法に適合するかどうかの
判断と責任は、利用者に帰属します。

なお、ここに書いたのは**本ツールの想定用途**であり、ソースコードのライセンス条件では
ありません。コードそのものは上記のとおり MIT / Apache-2.0 のデュアルライセンスで、
その許諾範囲は本節によって狭められるものではありません。
