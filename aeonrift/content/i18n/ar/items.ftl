# Aeonrift — توطين العناصر (ar)
#
# عنصر نائب. حتى هذه المرحلة من المحتوى لا توجد عناصر مُطلقة:
# يحتوي content/items/ على هيكل README فقط (لا توجد بعدُ مواصفات *.ron للعناصر).
#
# عندما تصل العناصر (ملف *.ron لكل عنصر، مُتحقَّق منه بالمخطط مثل الأبطال)،
# أضِف رسالة واحدة لكل مُعرِّف عنصر هنا وفق النمط أدناه:
#
#   item-<slug> = <الاسم المُوطَّن>
#       .description = <ملخّص أثر من سطر واحد>
#       .flavor = <نص نكهة>
#
# تسميات الفئات أدناه سلاسل واجهة ثابتة ويمكن ترجمتها الآن بأمان.

## فئات متجر العناصر (تستخدمها لوحة متجر شاشة اللعب)

item-category-starter = عناصر البداية
item-category-attack = هجوم
item-category-magic = سحر
item-category-defense = دفاع
item-category-boots = أحذية
item-category-consumable = مستهلكات
item-category-legendary = أسطوري

## واجهة المتجر

item-shop-title = المتجر
item-shop-buy = شراء
item-shop-sell = بيع
item-shop-undo = تراجع
item-shop-cost = { $gold } ذهب
item-shop-owned = مملوك
item-shop-cannot-afford = لا يكفي الذهب
item-shop-search = ابحث عن العناصر…
