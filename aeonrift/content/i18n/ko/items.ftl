# Aeonrift — 아이템 현지화 (ko)
#
# 플레이스홀더. 이번 콘텐츠 단계에서는 출시된 아이템이 없습니다:
# content/items/ 에는 README 스캐폴드만 있습니다 (아직 *.ron 아이템 스펙 없음).
#
# 아이템이 추가되면 (챔피언처럼 스키마 검증되는 아이템당 하나의 *.ron),
# 아래 패턴에 따라 아이템 슬러그마다 메시지를 하나씩 추가하세요:
#
#   item-<slug> = <현지화된 이름>
#       .description = <한 줄 효과 요약>
#       .flavor = <플레이버 텍스트>
#
# 아래 카테고리 라벨은 안정적인 UI 문자열이며 지금 번역해도 안전합니다.

## 아이템 상점 카테고리 (HUD 상점 패널에서 사용)

item-category-starter = 시작 아이템
item-category-attack = 공격
item-category-magic = 마법
item-category-defense = 방어
item-category-boots = 신발
item-category-consumable = 소모품
item-category-legendary = 전설

## 상점 UI

item-shop-title = 상점
item-shop-buy = 구매
item-shop-sell = 판매
item-shop-undo = 되돌리기
item-shop-cost = { $gold } 골드
item-shop-owned = 보유 중
item-shop-cannot-afford = 골드가 부족합니다
item-shop-search = 아이템 검색…
