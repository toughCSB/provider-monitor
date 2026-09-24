# Provider Monitor

<div align="center">

![Provider Monitor](docs/screenshots/provider-monitor-banner.png)

[![macOS CI](https://github.com/toughCSB/provider-monitor/actions/workflows/ci.yml/badge.svg)](https://github.com/toughCSB/provider-monitor/actions/workflows/ci.yml)
[![Windows](https://github.com/toughCSB/provider-monitor/actions/workflows/windows.yml/badge.svg)](https://github.com/toughCSB/provider-monitor/actions/workflows/windows.yml)
![Platform](https://img.shields.io/badge/platform-macOS%2015%2B%20%C2%B7%20Windows%2011-black)
![License](https://img.shields.io/badge/license-MIT-green)

**코딩 어시스턴트의 한도가 얼마나 남았는지, 그리고 지금도 일하고 있는지를 화면 가장자리의 노치 하나로 보여줍니다.**

**macOS와 Windows가 같은 버전 번호, 같은 릴리스로 함께 나갑니다.**

</div>

## 스크린샷

| macOS | Windows |
| :---: | :---: |
| ![macOS 노치와 호버 카드](docs/screenshots/macos-hover-card.png) | ![Windows 노치와 호버 카드](windows/docs/screenshots/windows-hover-card.png) |
| 화면 오른쪽 가장자리의 노치와 OpenCode 호버 카드. 링 왼쪽 위의 작은 배지가 지금 읽고 있는 주기입니다 — **W** 주간, **M** 월간, **5h** 5시간. | 같은 화면을 Windows로 옮긴 포트와 Claude 호버 카드. |

둘 다 실제로 실행 중인 화면을 캡처한 것입니다. macOS 쪽은 이 맥에 Cursor가 설치돼 있지 않아 링이 다섯 개입니다.

## 무엇을 보여주나

- **잔여량이 기준입니다.** 링 아래 숫자는 **남은** 퍼센트입니다. 24% 쓴 플랜은 **76%** 로 보입니다. 색 밴드(초록·노랑·빨강)는 그대로 실제 사용량을 따릅니다. 설정 → 모양 → **링 숫자**에서 사용량 기준으로 되돌릴 수 있습니다.
- **리셋까지 남은 시간을 호버 카드 맨 위에 크게.** `2일 3시간`, `58분` 처럼 남은 시간으로 보여줍니다. 시계 시각을 보고 직접 빼지 않아도 됩니다.
- **링마다 주기 전환.** 호버 카드의 **표시 기준** 줄에서 그 provider의 링만 주간 / 월간 / 5시간 / 기본 중 하나로 바꿉니다. 다른 링은 움직이지 않습니다. 기본값은 주간 한도이고, 주간 창이 없는 provider(Cursor)는 그 provider의 기본 창을 씁니다.
- **provider별 원래 색.** Claude는 코랄, Codex는 초록, Grok은 보라 — 공식 풀컬러 마크가 있는 브랜드는 그 색을 쓰고, 흑백 로고만 공개한 브랜드는 구분용 색을 씁니다. 그래프 색 규칙은 원본 앱 그대로입니다.
- **한국어.** 앱 UI와 호버 카드, 설정 창이 한국어입니다. 원본 앱이 지원하던 다른 언어도 그대로 남아 있습니다.
- **Antigravity 5시간 한도.** Gemini와 Claude/GPT 두 갈래의 5시간 · 주간 한도를 각각 읽습니다.

## 노치 다루기

- **항상 위에** — 우클릭 메뉴에서 켜면 다른 앱이 전체 화면이어도 노치가 맨 위에 남습니다.
- **어디에 띄울지** — 모니터가 여러 대면 **주 디스플레이만** 또는 **모든 디스플레이** 중에 고르고, 디스플레이를 직접 골라 고정할 수도 있습니다.
- **옮기기** — ⌥를 누른 채 끌면 그 변을 따라 이동하고, 변마다 위치를 기억합니다. 설정의 **가운데로** 버튼이 현재 변의 중앙으로 되돌립니다.
- **크기** — 작게 / 보통 / 크게 프리셋과 슬라이더가 있습니다. 슬라이더는 노치 전체(링·글자·툴팁)를 같은 비율로 키우고 줄입니다.
- **아이콘 순서** — 설정에서 provider 순서를 바꾸면 노치가 그 순서대로 그립니다.
- **알림** — 한도가 80%와 100%를 넘을 때 각각 한 번 시스템 알림이 옵니다. provider별로 끌 수 있습니다.

## 업데이트 확인은 두 개입니다

설정 → 정보에 서로 다른 두 가지가 있습니다. 헷갈리기 쉬워서 일부러 나눠 두었습니다.

| | 무엇을 보나 | 무엇을 하나 |
| --- | --- | --- |
| **원본 앱** | 원본 Codenotch가 새 버전을 냈는지 | 알려주기만 합니다. 설치 경로가 없습니다 |
| **Provider Monitor** | 지금 쓰고 있는 이 빌드 | macOS: 실행 중인 빌드를 /Applications에 설치. Windows: 이 릴리스의 설치 프로그램을 내려받아 실행 |

원본 피드에는 **일부러** 설치 경로를 두지 않았습니다. 그 피드로 업데이트를 허용하면 원본이 새 버전을 낼 때마다 이 포크가 조용히 원본 앱으로 되돌아가고, 이름과 아이콘, 우리가 얹은 것들이 배경에서 사라집니다.

## 릴리스

한 릴리스에 두 플랫폼이 함께 실립니다. 같은 `v1.17.2` 태그에 macOS dmg와 Windows 설치 프로그램이 같이 붙습니다.

- **macOS** — [ProviderMonitor-1.17.2-unsigned.dmg](../../releases/download/v1.17.2/ProviderMonitor-1.17.2-unsigned.dmg) · 유니버설(Apple Silicon + Intel), macOS 15 이상
- **Windows** — [Provider-Monitor-Setup.exe](../../releases/latest/download/Provider-Monitor-Setup.exe) · Windows 11 (WebView2 런타임 기본 포함)

`main`의 최신 커밋을 바로 써보려면 [preview 릴리스](../../releases/tag/preview)에 push마다 새 dmg가 올라옵니다.

## 설치

### macOS

1. 위 dmg를 받아 열고, `Provider Monitor.app`을 **Applications** 폴더로 끌어다 놓습니다.
2. 이 빌드는 **ad-hoc 서명**이라 공증이 없습니다. 첫 실행은 Finder에서 앱을 **우클릭 → 열기**로 한 번 열어 줍니다.
3. macOS가 *손상되었습니다* 라고 하면 다운로드가 깨진 것이 아니라 격리 플래그 때문입니다. 한 번만 지워 주면 됩니다:

```sh
xattr -dr com.apple.quarantine "/Applications/Provider Monitor.app"
```

### Windows

1. `Provider-Monitor-Setup.exe`를 실행합니다. 현재 사용자용으로 설치되고 관리자 권한이 필요 없습니다. 시작 메뉴 바로가기와 제거 프로그램이 함께 생깁니다.
2. 서명이 없어 SmartScreen이 처음 한 번 막습니다. **추가 정보** → **실행**을 고르면 됩니다.
3. 제거는 설치 폴더의 `uninstall.exe`로 하거나 Windows 설정 → 앱에서 합니다.

## 빌드

### macOS

```sh
brew install xcodegen   # 최초 한 번
make run                # 생성 + Debug 빌드 + 실행
make test               # 유닛 테스트
make dmg-ci             # 서명 없이 배포용 dmg 만들기
```

Debug 빌드와 `make test`는 서명 인증서가 없어도 됩니다. `make release`(Developer ID 서명 + 공증)는 인증서와 `notarytool` 프로필이 있어야 하고 유지보수자만 실행합니다. 앱의 업데이트 확인은 이 저장소의 최신 GitHub 릴리스와 SHA-256을 사용합니다.

### Windows

```powershell
cd windows
cargo test --locked
cargo build --release --locked -p codenotch-hook --target-dir target/hook
cd codenotch
npx @tauri-apps/cli@2 build --config tauri.bundle.conf.json
```

트레이 메뉴, 데이터 폴더(%APPDATA%\codenotch), 아이콘 교체, provider별로 읽는 곳까지 자세한 내용은 [`windows/README.md`](windows/README.md)에 있습니다.

## 지원 프로바이더

macOS는 원본 앱의 provider를 전부 그대로 지원합니다. Windows 포트는 실제 Windows 자격증명
경로와 API까지 검증 가능한 12개 provider를 구현했으며, 이름만 보이는 가짜 항목은 추가하지
않았습니다.

Windows의 12개 수집기는 앱이 실행되는 동안 설치·로그인 상태를 계속 확인합니다. 지금 사용하지
않아 `absent`인 provider도 나중에 해당 CLI나 데스크톱 앱을 설치하고 로그인하면 별도 등록 없이
자동으로 감지되어 설정과 노치에 나타납니다.

| 프로바이더 | macOS | Windows | 읽는 곳 |
| --- | :---: | :---: | --- |
| **Claude Code** | ✅ | ✅ | Claude Code 로그인, Claude Desktop의 사용량 캐시, 자체 `/usage` |
| **Codex** | ✅ | ✅ | `~/.codex/auth.json` (읽기만) |
| **Cursor** | ✅ | ✅ | 에디터의 `state.vscdb` 세션 |
| **Antigravity** | ✅ | ✅ | 공식 `agy` CLI, 없으면 로컬 language server |
| **Grok** | ✅ | ✅ | `~/.grok/auth.json` |
| **OpenCode (Go)** | ✅ | ✅ | `~/.local/share/opencode/auth.json` |
| **Devin** | ✅ | ✅ | Devin/Windsurf의 로컬 읽기 전용 세션 |
| **Gemini API** | ✅ | — | Gemini CLI·OpenCode·Hermes의 로컬 토큰 기록과 선택적 월간 예산 |
| **GLM** | ✅ | ✅ | Z.ai Coding Plan, 이미 있는 키를 빌려 씀 |
| **MiniMax** | ✅ | — | 설정에 넣은 키 또는 앱 안에서 로그인 |
| **DeepSeek** | ✅ | — | 앱 안에서 직접 로그인 (브라우저 쿠키는 읽지 않음) |
| **Command Code** | ✅ | ✅ | `~/.commandcode/auth.json` |
| **GitHub Copilot** | ✅ | ✅ | 이미 로그인돼 있는 `gh` 세션 |
| **Kimi** | ✅ | ✅ | `~/.kimi-code/credentials/kimi-code.json` |
| **Kiro** | ✅ | ✅ | kiro-cli 세션 |
| **Ollama / LM Studio** | ✅ | — | 같은 맥에서 돌고 있는 로컬 런타임 |

대부분의 provider는 이미 그 맥에 로그인돼 있는 도구의 세션을 **읽기만** 합니다. 토큰을 복사하거나 갱신하지 않습니다. DeepSeek과 MiniMax만 예외로 앱 안에서 직접 로그인하며, 브라우저의 쿠키 저장소는 열지 않습니다. 쓰지 않는 provider는 설정에서 끄면 폴링을 멈추고 읽은 값을 지웁니다.

Windows에서 아직 수집기를 이식하지 않은 항목은 별도 브라우저 세션이 필요한 DeepSeek·MiniMax,
로컬 토큰 기록과 예산 설정이 필요한 Gemini API, 로컬 런타임 모델 구조가 필요한 Ollama·LM
Studio, 별도 계정 키를 쓰는 Ollama Cloud입니다. 이들도 같은 등록 구조에 수집기를 추가하면 이후
설치·로그인을 자동 감지하게 됩니다. 현재는 실제 값을 읽지 못하는 빈 설정 행만 미리 만들지 않은
상태입니다.

## 원본 Codenotch와의 관계

이 저장소는 [vinzdg/codenotch](https://github.com/vinzdg/codenotch)의 포크입니다. macOS 앱은 원본 코드베이스를 그대로 이어받았고, 이 포크가 얹은 것은 위에 적은 표시 방식과 주기 전환, 한국어, 노치 설정입니다. `windows/`의 Rust + Tauri 포트는 Swift 코드를 옮긴 것이 아니라 같은 화면과 규칙을 다시 구현한 별개의 코드입니다.

원본이 새 버전을 내면 merge로 가져옵니다(`git fetch upstream && git merge upstream/main`). 어떤 파일에서 어느 쪽을 택할지는 [`docs/upstream-sync.md`](docs/upstream-sync.md)에 정리해 두었습니다. macOS 쪽은 원본이 정본이고 우리가 일부러 다르게 만든 것만 유지하며, `windows/`는 합치지 않고 필요한 수정만 골라 옮깁니다.

## 라이선스

[MIT](LICENSE) © 2026 Vinz
