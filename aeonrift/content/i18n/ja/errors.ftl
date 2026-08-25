# Aeonrift — エラー / ステータスメッセージのローカライズ (ja)
#
# クライアントとサービス向けの、ユーザーに表示されるエラー・ステータス文字列。
# メッセージは実行可能な内容にとどめ、内部情報を漏らさないこと。

## 接続

error-network-lost = 接続が切れました。再接続を試みています…
error-network-timeout = サーバーが応答しませんでした。接続を確認してください。
error-server-unavailable = 現在サーバーを利用できません。しばらくしてからもう一度お試しください。
error-reconnecting = 再接続中… ({ $max } 回中 { $attempt } 回目)
error-reconnect-failed = マッチに再接続できませんでした。

## 認証

error-auth-required = 続けるにはサインインが必要です。
error-auth-invalid = ユーザー名またはパスワードが正しくありません。
error-auth-expired = セッションの有効期限が切れました。もう一度サインインしてください。
error-auth-banned = このアカウントは停止されています。

## マッチメイキング

error-match-declined = マッチはキャンセルされました：プレイヤーが辞退しました。
error-match-timeout = マッチはキャンセルされました：プレイヤーが応答しませんでした。
error-match-abandoned = マッチを離脱したため、ペナルティを受ける可能性があります。
error-queue-dodge = プレイヤーが回避しました。ロビーに戻ります。

## チャンピオン選択 / ゲームプレイ

error-champion-locked = そのチャンピオンは利用できません。
error-champion-banned = そのチャンピオンはこの試合でBANされています。
error-champion-already-picked = そのチャンピオンはすでに選ばれています。
error-invalid-target = 無効な対象です。
error-out-of-range = 対象が射程外です。
error-not-enough-mana = マナが足りません。
error-not-enough-energy = エネルギーが足りません。
error-ability-on-cooldown = アビリティはクールダウン中です ({ $seconds }秒)。

## ショップ / 経済

error-not-enough-gold = ゴールドが足りません。
error-item-not-available = そのアイテムは利用できません。
error-inventory-full = インベントリがいっぱいです。

## コンテンツ / 読み込み

error-content-load-failed = ゲームコンテンツの読み込みに失敗しました。クライアントを再起動してください。
error-content-corrupt = ゲームファイルが破損しているようです。修復を推奨します。
error-version-mismatch = クライアントが最新ではありません。続けるには更新してください。

## 汎用

error-unknown = 問題が発生しました。もう一度お試しください。
error-retry = 再試行
error-report = 問題を報告
error-details = エラーコード: { $code }
