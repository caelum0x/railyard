# Aeonrift — Локализация предметов (ru)
#
# ЗАГЛУШКА. На момент этого прохода по контенту предметов ещё нет:
# content/items/ содержит лишь каркас README (нет ни одного *.ron со спецификациями предметов).
#
# Когда предметы появятся (один *.ron на предмет, с проверкой схемы, как у чемпионов),
# добавьте по одному сообщению на слаг предмета по образцу ниже:
#
#   item-<slug> = <Локализованное имя>
#       .description = <Однострочное описание эффекта>
#       .flavor = <Художественный текст>
#
# Ярлыки категорий ниже — это стабильные строки интерфейса, их можно переводить уже сейчас.

## Категории магазина предметов (используются панелью магазина в HUD)

item-category-starter = Стартовые предметы
item-category-attack = Атака
item-category-magic = Магия
item-category-defense = Защита
item-category-boots = Ботинки
item-category-consumable = Расходуемые
item-category-legendary = Легендарные

## Интерфейс магазина

item-shop-title = Магазин
item-shop-buy = Купить
item-shop-sell = Продать
item-shop-undo = Отменить
item-shop-cost = { $gold } золота
item-shop-owned = В наличии
item-shop-cannot-afford = Недостаточно золота
item-shop-search = Поиск предметов…
