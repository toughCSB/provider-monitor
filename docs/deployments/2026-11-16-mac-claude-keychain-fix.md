# Mac Claude 계정 등록의 기본 키체인 오류

## 재현과 원인

1.18.1에서 Claude 계정 추가 중 “키체인 발견할 수 없음 — ‘csb_mac’을(를) 저장할 키체인을 찾을 수 없습니다” 창이 나타났습니다.

`ManagedAccountCLI.environment`가 공식 CLI에 전달하는 `HOME`을 앱의 계정 전용 폴더로 바꾸고 있었습니다. macOS Security.framework는 기본 키체인 경로를 해석할 때 이 환경의 영향을 받습니다. 계정 격리에 필요한 값은 `CLAUDE_CONFIG_DIR` 또는 `CODEX_HOME`이며, 사용자 홈 자체를 바꿀 필요가 없습니다.

사용자 상호작용을 비활성화한 읽기 전용 `SecKeychainCopyDefault` 검사 결과:

| 실행 환경 | 결과 |
|---|---|
| 원래 HOME | status 0, 기존 login.keychain-db 존재 |
| 1.18.1과 같이 HOME을 전용 폴더로 변경 | status -25307, 기본 키체인 없음 |
| 원래 HOME 유지, CLAUDE_CONFIG_DIR만 전용 폴더로 지정 | status 0, 같은 기존 키체인 정상 확인 |

`security error -25307`도 “A default keychain could not be found.”로 확인했습니다. 실제 사용자 키체인의 파일 손상이나 삭제가 원인이 아니므로 기본 키체인 재설정·삭제·권한 변경은 수행하지 않았습니다.

## 수정 및 자동 검증

- 공식 CLI 자식 프로세스의 HOME 덮어쓰기를 제거했습니다.
- 계정별 CODEX_HOME·CLAUDE_CONFIG_DIR 및 상속된 공급자 인증 환경 변수 제거는 유지했습니다.
- 실제 로그인 transport를 통과하는 회귀 테스트를 먼저 추가했습니다. 수정 전에는 임시 계정 폴더가 HOME으로 관측되어 실패했고, 수정 후에는 실제 사용자 홈과 별도 계정 폴더가 함께 관측되어 통과했습니다.
- Codex RPC 경로에도 동일한 홈 보존·계정 폴더 분리 검증을 추가했습니다.
- 전체 테스트: 1,718개 실행, 3개 제외, 실패 0개.
- Mac 수정 버전: 1.18.2 / build 28. 기존 Codex 등록 및 사용자 설정을 보존합니다.

실패했던 Claude 등록 시도의 공식 `auth status`는 `loggedIn=false`였습니다. 당시 남은 폴더나 이전 인증 메타데이터를 로그인 성공으로 취급하지 않았습니다.

## 설치 후 실제 검증

- `/Applications/Provider Monitor.app`을 1.18.2 / build 28로 설치하고 실행했습니다. 기존 설치본은 `build/before-keychain-1182.noindex/20261116-115209`에 백업했습니다.
- 실제 앱의 Claude 계정 추가 버튼으로 다시 로그인했습니다. Aside의 공식 Claude 로그인 성공 화면, 앱의 이메일 확인 단계, Claude 1 등록 완료 화면을 확인했습니다. 키체인 재설정은 수행하지 않았습니다.
- 등록한 계정의 실제 새 사용량이 앱에 저장됐습니다: session 사용 0%(잔여 100%), weekly_all 사용 82%(잔여 18%), weekly_fable 사용 43%(잔여 57%). 기존 Codex 두 계정도 새 읽기를 유지했습니다.
- 실행 파일 SHA-256: `6c8c933b1f6e7c16871ab982275e52c39f14f84e0af5a2ad46917743b14e4255`.
