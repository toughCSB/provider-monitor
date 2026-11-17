# 다중 계정 사용량 모니터링: 도구 비교와 권장 설계 (2026-11-15)

범위: 공식 문서, 모니터링·계정 전환 도구의 공개 문서와 소스, Provider Monitor의 현재 구현. 외부 도구를 설치하거나 사용자 로그인·설정·실행 중 세션을 바꾸지 않았다. 도구 비교는 조사 시점 공개 브랜치 기준이며 설치된 릴리스의 동작을 실험한 결과가 아니다. 아래 `추론`과 `제안`은 확인된 동작과 구별한다.

## 결론

**제안:** Provider Monitor의 기본 흐름을 `계정 추가 → 공식 로그인 → 계정 확인 → 모니터에 표시`로 바꾼다. 앱이 계정별 저장 공간을 만들고 관리하며 사용자가 폴더를 만들거나 경로를 입력하게 하지 않는다. CodexBar와 Win-CodexBar의 관리 계정 구조를 참고하되, 조회와 실제 CLI 계정 전환은 분리한다. Codex는 공식 app-server 조회 경로를 우선 검증하고, Claude는 공식 CLI 경유 조회의 범위·안정성을 따로 검증한다. 두 제품의 인증 구현을 동일하게 취급하지 않는다.

현재 Aside 연동은 작동한 보조 경로로 보존할 수 있지만 브라우저 내부 저장 형식을 세 기기의 공통 인증 기반으로 삼는 것은 권하지 않는다. 토큰을 복사해 Mac/SV/HOME에서 따로 갱신시키는 방법도 기본안으로 삼지 않는다. 이는 안정성에 관한 설계 판단이며, 브라우저 방식이 항상 실패하거나 인증 파일 복사가 언제나 금지된다는 주장이 아니다.

## 지금 설정 화면의 의미와 문제

현재 로컬 소스 `windows/codenotch/ui/settings.html`의 `Second account directories`와 `get_second_enabled` 호출을 확인했다.

| 화면 | 실제 의미 |
|---|---|
| `C:\Users\you\.codex-work` | 입력 예시(placeholder). 실제 연결된 폴더 표시가 아니다. |
| `Add existing` | 이미 다른 계정으로 로그인된 폴더를 연결한다. 새 로그인이나 계정 보관을 수행하지 않는다. |
| `Auto` | `.codex-*`·`.claude-*` 로컬 폴더 검색을 켠다. ChatGPT 웹의 계정 전환 목록을 가져오는 버튼이 아니다. |
| `Disconnect` / `Reconnect` | 두 번째 슬롯의 활성화 설정이다. 인증 성공이나 실제 계정 존재를 증명하지 않는다. |

**확인된 UI 문제:** 계정이 발견되지 않아도 `enabled=true`이면 Disconnect라고 표시된다. 사용자가 로그인 프로필을 따로 만든 적이 없는데 고급 폴더 연결 기능을 기본 계정 추가 화면으로 제시한 것이 혼란의 원인이다. 기본 상태는 `두 번째 계정 미등록`이어야 하며, 인증 성공 후에만 계정 이름과 조회 상태를 표시해야 한다.

## 다른 도구 비교

| 도구 | 확인한 방식 | 이 프로젝트에 대한 판단 |
|---|---|---|
| CodexBar | 기본 CLI 계정과 앱 관리 계정을 구분한다. 앱 소스는 관리 디렉터리를 만들고 해당 경로로 로그인 실행을 위임한다. 기존 폴더 지정은 고급 옵션이다. | **Codex 등록 UX와 계정별 격리의 주 참고 모델.** [문서](https://github.com/steipete/CodexBar/blob/main/docs/codex.md), [관리 계정 소스](https://github.com/steipete/CodexBar/blob/1f6da9bec44455288ddee5ef6f3617a803d8eb2a/Sources/CodexBar/Providers/Codex/ManagedCodexAccountService.swift#L42-L85) |
| Win-CodexBar | Windows의 계정 등록·사용량·전환을 별도로 제공한다. Codex 로그인 실행기는 별도 CODEX_HOME에서 공식 바이너리의 login을 실행한다. 계정 레지스트리는 DPAPI로 보호한다. | **Windows에서 수동 경로 없이 등록할 수 있다는 구체적인 구현 사례.** 전체 전환 기능까지 복제할 필요는 없다. [설정](https://github.com/nesszer/Win-CodexBar/blob/main/docs/CONFIGURATION.md#codex-accounts), [로그인 소스](https://github.com/nesszer/Win-CodexBar/blob/fd06294db4366bd1008ce2d32d6b6e7c0bb5b0d7/rust/src/codex_accounts/login_runner.rs#L79-L108) |
| ccswap | 각 CLI 로그인 직후 별도로 저장한다. Codex 전환은 auth.json을 교체하며 재시작을 안내한다. Codex는 file 저장소를 요구한다. 사용량 스냅샷에는 인증 정보가 없으므로 모니터가 읽는 연동도 가능하다. | **CLI 계정 전환까지 원할 때 검토할 후보.** 현재 장기 실행 세션에 자동 전환을 먼저 도입하는 것은 권하지 않는다. [Codex 문서](https://github.com/errhythm/ccswap/blob/main/docs/codex.md) |
| claude-swap | Claude 계정을 등록·전환하고 사용량을 제공한다. CodexBar는 별도 어댑터로 JSON 결과를 읽으며 이 통합에서 자격 증명 저장소를 직접 열지 않는다. | **계정 관리와 표시를 분리하는 계약의 참고 사례.** 도구가 존재한다는 사실은 Anthropic의 허용을 증명하지 않는다. [도구](https://github.com/realiti4/claude-swap), [CodexBar 어댑터](https://github.com/steipete/CodexBar/blob/main/docs/claude.md#claude-swap-accounts-opt-in) |
| Quotio | 여러 계정·사용량을 관리하며 CLIProxyAPI도 운영한다. 프록시를 실행하지 않는 독립 모니터링 모드가 있다. | 계정 중심 화면은 참고할 만하다. 단순 한도 표시에 요청 라우팅까지 추가할 이유는 없다. [macOS 앱 문서](https://github.com/nguyenphutrong/quotio/blob/master/apps/macos/README.md) |
| CC Switch | API 공급자와 CLI 설정·MCP 등을 관리하며 ChatGPT 등 계정 로그인 기능도 제공한다. | 여러 공급자 설정을 함께 바꾸려는 목적에 맞는다. 두 계정의 잔여량만 표시하는 문제에는 범위가 더 크다. [프로젝트 문서](https://github.com/farion1231/cc-switch) |

소스 확인 기준: CodexBar 트리 `1f6da9bec44455288ddee5ef6f3617a803d8eb2a`, Win-CodexBar 트리 `fd06294db4366bd1008ce2d32d6b6e7c0bb5b0d7`, ccswap 트리 `775567c93ec3511b07e4bd16a28b008f9c90fe15`. 브랜치 문서는 후속 변경될 수 있다.

## 권장 사용자 흐름

1. `Codex 계정 추가`를 누르면 앱이 모니터 전용 인증 공간을 자동 생성한다.
2. 공식 로그인 창에서 원하는 계정으로 인증한다. 웹에 로그인되어 있어도 모니터용 등록은 처음 한 번 필요할 수 있다.
3. 반환된 계정과 워크스페이스를 확인한다. 기존 계정과 같으면 중복 추가하지 않고 알린다.
4. `Codex 1 · 개인`, `Codex 2 · 업무`처럼 이름·번호·아이콘 색상을 고정한다.
5. 표시 계정을 바꾸거나 새로고침해도 평소 CLI의 활성 로그인과 기존 세션은 건드리지 않는다.
6. `CLI에서 이 계정 사용`은 별도 요청이 있을 때 추가할 독립 기능으로 둔다. 현재 계정 가져오기는 기본 경로를 참조하는 것과 독립 로그인 등록을 명확히 구분하며, 갱신 토큰을 복사해 독립 계정처럼 다루지 않는다.

기존 폴더 입력은 `고급 → 이미 만든 로그인 폴더 연결`로 남긴다. Aside 연결에는 `브라우저에서 읽음`이라는 출처를 표시하고, 실패하면 해당 계정에 재연결 안내를 보여 준다. 다른 계정의 정상 수치로 대신 채우지 않는다.

## Mac·SV·HOME의 배치

**독립 동작을 우선하는 기본안:** 세 기기에서 동일한 계정 등록 UI를 제공하고 각 기기에서 각 계정을 한 번씩 인증한다. 이름·색상·표시 순서는 동기화할 수 있지만 인증 파일 자체는 공유하지 않는다. 한 기기가 꺼져도 나머지의 조회가 독립적으로 가능하다.

**등록 횟수를 줄이는 선택안:** 상시 켜져 있는 수집 기기를 정하고 나머지 기기에는 계정 ID·사용량·측정 시각·상태만 전달한다. 자격 증명은 수집 기기에 남긴다. 수집 기기가 꺼지면 마지막 값의 나이를 표시해야 한다. 사용자의 상시 가동 기기와 공유 방식이 정해지기 전에는 이를 기본안으로 확정하지 않는다.

두 안 모두 계정의 잔여 한도와 기기별 활동을 분리한다. 동일 계정의 한도 응답을 기기별로 더하거나 평균내지 않고, 같은 계정·워크스페이스의 가장 최근 유효 관측값을 선택한다. 이는 제안하는 데이터 모델 규칙이다.

## 구현 전 완료 조건

- 기본 UI에서 경로를 입력하지 않고 두 계정을 등록할 수 있다.
- 모니터에 A·B를 등록한 뒤 일반 CLI를 A↔B로 로그인해도 두 모니터 계정의 정체성이 바뀌지 않는다.
- 동시 갱신, 만료, 로그아웃, 브라우저 종료, 재부팅에서 계정별 상태가 섞이지 않는다.
- 갱신 담당을 계정별로 하나로 정하고, 만료·권한 거부·서버 제한을 구별한다.
- CLI 활성 계정, 기존 세션 PID·작업 경로·대화 ID를 유지한다.
- Mac/SV/HOME에서 실제 공식 사용량과 비교한다. Windows는 설치 여부만으로 완료 처리하지 않고 실제 두 계정 표시와 갱신을 확인한다.
- Claude는 공식 CLI의 조회 가능 범위·해당 배포 형태의 인증 조건을 먼저 확인한다. 구현이 존재하는 외부 도구를 근거로 허용이나 안정성을 단정하지 않는다.

이하 공식 근거 메모는 별도 조사 결과다. 해당 조사에서는 사용자 인증 파일이나 실제 계정을 열지 않았다.

## Codex: 저장 단위와 로그인

- **확인:** Codex CLI와 IDE 확장은 로그인 캐시를 공유한다. `CODEX_HOME`의 기본값은 `~/.codex`이고, `cli_auth_credentials_store`의 공개 소스 기본값은 `file`, 즉 `CODEX_HOME/auth.json`이다. 설정값 `keyring`은 OS 저장소가 없으면 실패하고, `auto`는 OS 저장소를 우선하다 파일로 대체하며, `ephemeral`은 프로세스 메모리만 쓴다. macOS라는 이유만으로 CLI 기본 저장소가 Keychain이라고 볼 수 없다. [인증 문서](https://learn.chatgpt.com/docs/auth), [저장 모드 소스](https://github.com/openai/codex/blob/main/codex-rs/config/src/types.rs#L2393-L2419)
- **확인:** 공개 소스의 Keyring 저장 키는 `CODEX_HOME` 경로로 계산된다. 따라서 Keyring 모드에서도 별도 `CODEX_HOME`은 별도 자격 증명 식별자다. [`storage.rs`](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/storage.rs#L2193-L2326)
- **확인:** `--profile`은 `$CODEX_HOME/<name>.config.toml` 설정 계층을 선택한다. 문서는 이를 계정 저장소 분리 기능이라고 설명하지 않는다. **추론:** 동일한 `CODEX_HOME`에서 `--profile`만 바꿔서는 독립적인 ChatGPT 로그인 두 개가 보존되지 않는다. [설정 기본](https://learn.chatgpt.com/docs/config-file/config-basic), [설정 참조](https://learn.chatgpt.com/docs/config-file/config-reference)
- **확인:** 현재 공개 소스의 브라우저·device-code `codex login` 경로는 새 로그인 흐름 전에 같은 저장소의 기존 인증을 삭제·철회하려고 시도한다. 새 흐름 실패 시에도 기존 인증이 사라질 수 있으므로 이를 계정 추가로 취급할 수 없다. [`login.rs`](https://github.com/openai/codex/blob/main/codex-rs/cli/src/login.rs#L1906-L1985). 브라우저의 ChatGPT 계정 전환이 CLI의 별도 계정 슬롯을 만든다는 공식 근거는 없다.
- **확인:** OpenAI는 headless 기기에 `codex login --device-auth`를 우선 안내하고, `auth.json` 복사는 fallback으로 문서화한다. 복사본은 비밀번호처럼 취급해야 한다. Codex는 실행 중 토큰을 자동 갱신한다. [인증 문서](https://learn.chatgpt.com/docs/auth)
- **위험·추론:** 같은 갱신 토큰을 여러 호스트·저장소에 복제해 독립적으로 갱신시키면 복사본이 뒤처질 수 있다. 소스는 갱신된 인증을 저장하고 동일 저장소 내 변경 전후를 비교하며 `refresh_token_reused` 오류를 처리한다. 이는 **복제본 간 안전한 동기화 보장**이 아니다. 모든 복사가 반드시 실패한다는 뜻도 아니다. [갱신 소스](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/manager.rs), [저장소 비교](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/storage.rs#L2046-L2065)

## 공식 조회 범위와 제3자 연동

- **확인:** Codex app-server JSON-RPC는 현재 로그인 하나의 `account/read`(계정·플랜), `account/rateLimits/read`(버킷별 `usedPercent`, 기간, `resetsAt`), `account/usage/read`(토큰 활동 요약)를 문서화한다. `account/usage/read`는 Codex 서비스 기반 인증이 필요하며 API key-only·Bedrock 인증은 지원하지 않는다. 이는 **해당 app-server 인스턴스의 인증 컨텍스트** 조회이지 여러 ChatGPT 계정을 한 번에 검색하는 API로 문서화되어 있지 않다. [app-server 인증·조회](https://learn.chatgpt.com/docs/app-server#auth-endpoints)
- **범위 제한:** OpenAI는 기존 app-server 인증을 로컬 또는 오픈소스 앱에서 계속 사용할 수 있다고 하지만 `Sign in with ChatGPT`로 전환을 권장하고, app-server 인증은 상용·호스팅 서비스에 허용된 적 없다고 명시한다. `Sign in with ChatGPT`는 제3자 앱을 위한 별도의 공식 통합 경로다. **불확실:** 단순 모니터가 CLI 로그인을 내부에서 실행하고 app-server로 구독 제한량을 읽는 특정 배포 형태의 허용 여부를 이 문서만으로 포괄 승인할 수 없다. [app-server 문서](https://learn.chatgpt.com/docs/app-server#auth-endpoints), [Sign in with ChatGPT](https://developers.openai.com/siwc)

## Claude Code: 저장소·조회·정책

- **확인:** macOS Claude Code 자격 증명은 기본적으로 Keychain에 저장된다. Keychain 쓰기가 거부되면 구독 로그인은 `~/.claude/.credentials.json` (`0600`)으로 대체된다. Linux는 같은 파일·권한, Windows는 `%USERPROFILE%\.claude\.credentials.json`과 사용자 프로필 ACL을 사용한다. `CLAUDE_CONFIG_DIR`가 다르면 파일 위치와 macOS Keychain 항목도 달라진다. 따라서 별도 디렉터리는 사용자에게 수동 생성을 요구하지 않고 앱이 관리할 수 있는 **설계 후보**이나, 이 조사에서 실제 다중 로그인 동작을 실행 검증하지 않았다. [Claude Code 인증](https://code.claude.com/docs/en/authentication#credential-management)
- **확인:** Claude Code는 `/login`, `/logout`으로 자격 증명을 관리하며, `/logout`은 다음 실행의 로그인·초기 설정을 다시 요구한다. 공식 문서는 `/usage`를 **대화형** 플랜 제한·현재 상태 확인 명령으로 소개한다. 개인 구독의 실시간 잔여량을 제3자 앱이 OAuth 토큰으로 직접 조회할 수 있는 공개 API는 이번 공식 문서 조사에서 확인하지 못했다. [인증](https://code.claude.com/docs/en/authentication), [명령어 안내](https://support.claude.com/en/articles/14553413-claude-code-cheatsheet)
- **확인:** Claude Code Analytics Admin API는 Claude Console 조직의 일별 집계(최대 약 1시간 지연)를 제공하며, Pro·Max·Team·Enterprise 등 **Claude 구독 로그인 사용량은 반환하지 않는다**. OpenTelemetry는 별도 설정으로 실행·토큰·비용 등의 활동 지표를 내보내는 공식 경로지만 구독 잔여 한도 API는 아니다. [Analytics API](https://platform.claude.com/docs/en/manage-claude/claude-code-analytics-api), [OpenTelemetry](https://code.claude.com/docs/en/monitoring-usage)
- **정책:** Anthropic은 제3자 개발자가 자기 앱에 Claude.ai 로그인을 제공하거나 구독 자격 증명으로 사용자 대신 요청을 중계하는 것, Claude.ai 자격 증명·세션 토큰을 수집·저장·중계하는 것을 허용하지 않는다. 사용자가 **수정되지 않은 Claude Code 바이너리**에 자신의 계정으로 로그인하는 것은 별도로 허용한다. **불확실:** 제3자 모니터가 Claude Code의 대화형 `/usage` 결과를 자동 회수하는 세부 방식의 허용·안정성은 이 조항만으로 확정할 수 없다. [Anthropic 법률·준수 문서](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)

## 설계에 바로 반영할 경계

**제안:** 계정 추가는 각 호스트·계정마다 별도 인증 컨텍스트에서 공식 CLI 로그인 흐름을 사용자에게 맡기고, 앱은 디렉터리 경로와 계정 표시 이름만 관리하는 방향을 검토한다. Codex의 `--profile`이나 Aside 웹 세션을 계정 슬롯으로 간주하지 않는다. 갱신 토큰을 Mac/SV/HOME으로 복제해 각자 갱신시키는 구조는 피하고, 호스트 간 공유가 필요하면 인증 정보 없는 사용량 스냅샷만 전송하는 방안을 별도 검토한다. Claude 구독 잔여량은 공식 `/usage` 화면 확인 수준과 실제 자동화 가능 범위를 분리해 표기한다. 이 제안의 제품별 실현·약관 적합성은 구현 전 추가 확인이 필요하다.
