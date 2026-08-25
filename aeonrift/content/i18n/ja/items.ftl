# Aeonrift — アイテムのローカライズ (ja)
#
# プレースホルダー。今回のコンテンツ更新時点では実装済みのアイテムはありません。
# content/items/ には README の雛形のみが含まれます（*.ron のアイテム仕様はまだありません）。
#
# アイテムが追加されたら（チャンピオンと同様にスキーマ検証された *.ron を1アイテムにつき1つ）、
# 以下のパターンに従ってアイテムスラッグごとに1メッセージをここに追加してください:
#
#   item-<slug> = <ローカライズされた名前>
#       .description = <効果の一行要約>
#       .flavor = <フレーバーテキスト>
#
# 以下のカテゴリラベルは安定したUI文字列であり、今すぐ翻訳しても安全です。

## アイテムショップのカテゴリ（HUDのショップパネルで使用）

item-category-starter = スターターアイテム
item-category-attack = 攻撃
item-category-magic = 魔法
item-category-defense = 防御
item-category-boots = ブーツ
item-category-consumable = 消耗品
item-category-legendary = レジェンダリー

## ショップUI

item-shop-title = ショップ
item-shop-buy = 購入
item-shop-sell = 売却
item-shop-undo = 元に戻す
item-shop-cost = { $gold } ゴールド
item-shop-owned = 所持済み
item-shop-cannot-afford = ゴールドが足りません
item-shop-search = アイテムを検索…
