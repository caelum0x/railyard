# Aeonrift — 오류 / 상태 메시지 현지화 (ko)
#
# 클라이언트와 서비스를 위한 사용자 대상 오류 및 상태 문자열.
# 메시지는 실행 가능하게 유지하고, 내부 세부 정보는 절대 노출하지 마세요.

## 연결

error-network-lost = 연결이 끊겼습니다. 재연결을 시도하는 중…
error-network-timeout = 서버가 응답하지 않았습니다. 연결 상태를 확인해 주세요.
error-server-unavailable = 지금은 서버를 이용할 수 없습니다. 잠시 후 다시 시도해 주세요.
error-reconnecting = 재연결 중… ({ $attempt } / { $max })
error-reconnect-failed = 매치에 재연결할 수 없습니다.

## 인증

error-auth-required = 계속하려면 로그인해야 합니다.
error-auth-invalid = 사용자 이름 또는 비밀번호가 올바르지 않습니다.
error-auth-expired = 세션이 만료되었습니다. 다시 로그인해 주세요.
error-auth-banned = 이 계정은 정지되었습니다.

## 매치메이킹

error-match-declined = 매치가 취소되었습니다: 한 플레이어가 거절했습니다.
error-match-timeout = 매치가 취소되었습니다: 한 플레이어가 응답하지 않았습니다.
error-match-abandoned = 매치를 이탈하여 제재를 받을 수 있습니다.
error-queue-dodge = 한 플레이어가 이탈했습니다. 로비로 돌아갑니다.

## 챔피언 선택 / 게임 플레이

error-champion-locked = 해당 챔피언은 사용할 수 없습니다.
error-champion-banned = 해당 챔피언은 이번 게임에서 금지되었습니다.
error-champion-already-picked = 해당 챔피언은 이미 선택되었습니다.
error-invalid-target = 잘못된 대상입니다.
error-out-of-range = 대상이 사거리를 벗어났습니다.
error-not-enough-mana = 마나가 부족합니다.
error-not-enough-energy = 기력이 부족합니다.
error-ability-on-cooldown = 스킬 재사용 대기 중입니다 ({ $seconds }초).

## 상점 / 경제

error-not-enough-gold = 골드가 부족합니다.
error-item-not-available = 해당 아이템은 사용할 수 없습니다.
error-inventory-full = 인벤토리가 가득 찼습니다.

## 콘텐츠 / 로딩

error-content-load-failed = 게임 콘텐츠를 불러오지 못했습니다. 클라이언트를 다시 시작해 주세요.
error-content-corrupt = 게임 파일이 손상된 것으로 보입니다. 복구를 권장합니다.
error-version-mismatch = 클라이언트가 최신 버전이 아닙니다. 계속하려면 업데이트해 주세요.

## 일반

error-unknown = 문제가 발생했습니다. 다시 시도해 주세요.
error-retry = 다시 시도
error-report = 문제 신고
error-details = 오류 코드: { $code }
