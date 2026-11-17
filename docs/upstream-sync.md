# upstream(원본 Codenotch) 반영 전략

이 포크는 [vinzdg/codenotch](https://github.com/vinzdg/codenotch)의 포크다. 원본은 계속 업데이트되므로,
그 변경을 어떻게 가져올지가 유지보수의 전부라고 해도 된다. 2026-09-17에 1.13.1을 병합하면서 확정한
방식을 여기에 남긴다.

## 결론

- **merge로 가져온다.** `git fetch upstream && git merge upstream/main`. rebase는 쓰지 않는다 —
  포크에 이미 공개한 커밋 해시가 전부 바뀌고, 그 위에 올린 빌드·릴리스 이력이 끊긴다.
- **macOS(`Sources/`)는 upstream이 정본이다.** 충돌하면 upstream 것을 기본으로 채택하고, 우리가
  일부러 다르게 만든 것(앱 이름·번들 ID, 잔여량 기준, 링 주기 전환, 한국어)만 우리 것을 유지한다.
- **`windows/`는 upstream과 합치지 않는다.** upstream에도 Windows 포트가 있지만 이 포크의 포트와는
  같은 기능을 각자 구현한 **다른 코드**다. 파일 단위로 섞으면 컴파일도 안 되고, 애써 맞춘 화면도
  무너진다. upstream Windows 수정 중 필요한 것만 골라 손으로 옮긴다.
- **Windows 검증은 Windows 호스트에서 한다.** 현재 GitHub workflow는 삭제된 상태다.
  SV 등 연결 가능한 Windows에서 `cargo test --locked -p codenotch`와 NSIS 빌드를 실행하고,
  사용자 데스크톱 세션에서 설치·실행·사용량을 확인한다. Mac의 Node 검사만으로 네이티브 검증을 대체하지 않는다.

## 파일별 충돌 정책

| 대상 | 정책 | 이유 |
| --- | --- | --- |
| `Sources/**` (macOS 앱) | upstream 채택, 우리 의도적 변경은 유지 | 같은 코드베이스의 포크다 |
| `Sources/Settings/RingCadence.swift` 같은 포크 전용 파일 | 우리 것 유지 | upstream에 없다 |
| `Sources/Localizable.xcstrings` | 키 **합집합**. 같은 키는 `ko`가 있는 우리 것 유지 | 번역을 잃지 않는다 |
| `.github/workflows/**` | 현재 삭제 유지 | 복원은 별도 결정이다. upstream 병합만으로 복원하지 않는다 |
| `site/**` | 우리 쪽은 비워 둔다(삭제를 유지한다) | upstream이 서명·공증한 dmg와 appcast를 넣는 자리라 포크에서는 쓸 일이 없다. 이 포크는 GitHub Pages를 쓰지 않는다 |
| `windows/**` | **현재 작업본 보존** | 미커밋 구현이 있으므로 `git checkout HEAD -- windows/`로 덮어쓰지 않는다 |
| `project.yml` | downstream 버전·번들 ID·GitHub 릴리스 업데이트 정책은 우리 것, upstream 설정 변경은 채택 | |

같은 키라도 **upstream이 더 새것이면 upstream 값을 택한다.** 이번에 `%lld%% of its %@ limit used.`의
zh-Hans 번역이 순서만 바꿔 놓은 채 남아 있었고(1.13.1에서 upstream이 `%1$lld`/`%2$@`로 고쳤다),
우리 값을 유지하는 규칙 때문에 그 수정이 되돌아가 `CatalogFormatTests`가 잡아냈다. 그건 #237의
중국어 크래시 그 자체다.

## 절차

```bash
git fetch upstream --tags
git switch -c merge/upstream-<버전> main
git merge upstream/main
# 충돌 해결: 위 표
git diff --name-only --diff-filter=U      # 남은 충돌이 0인지 확인
```

번역 누락은 손으로 찾지 않는다. Xcode에게 물어본다:

```bash
D=$(mktemp -d)
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcodebuild \
  -exportLocalizations -localizationPath "$D" \
  -project ProviderMonitor.xcodeproj -exportLanguage en
```

이 명령은 소스에서 참조되는 문자열을 추출해 `Sources/Localizable.xcstrings`에 없는 키를 직접
추가해 준다. 새로 들어온 키는 `ko` 번역이 없으므로 채워 넣는다.

검증:

```bash
make test          # 0 failures
```

Mac에서 공통 화면 검사를 실행한다. `make test`와 `make test-ci`에도 연결되어 있다:

```bash
make test-ui
```

## 이번 병합(1.13.1)에서 실제로 내린 판단

- `Sources/Sessions/CodexActivityMonitor.swift`: 우리는 호버 지연을 커서(증분 읽기)로 고쳤고,
  upstream은 역방향 256KB 윈도 스캔으로 다시 썼다. **upstream 것을 채택했다.** 성능 목표가 같고,
  이후 다듬어진 쪽이 upstream이며, 앞으로의 병합 비용이 0이 된다. 대신 1MB보다 뒤에 있는 라이프사이클
  이벤트는 찾지 않는 명시적 상한이 생긴다(진행 중 턴은 그대로 `busy`로 답한다).
- `windows/codenotch/src/traymenu.rs`, `windows/scripts/check-ui-scripts.mjs`는 upstream 쪽
  추가분이라 병합 결과에서 제외했다. 앞의 것은 우리 `tray.rs`와 맞물려 있어 단독으로는 컴파일되지
  않고, 뒤의 것은 upstream `windows.yml`이 부르는 스크립트라 남겨 두었다.
- 카탈로그는 키 합집합 668개에서 시작해 Xcode 추출로 819개가 됐다. 자세한 수치는 커밋 메시지에 있다.

`site/Codenotch.dmg`와 `site/appcast.xml`은 upstream이 자기 릴리스마다 갱신해 넣는
배포 산출물이다(합쳐서 10MB). 이 포크는 Pages를 켜지 않았고 공증도 할 수 없어 쓸 일이
없으므로 삭제했다. 다음 병합에서 modify/delete 충돌로 다시 나타나면 삭제를 유지하면 된다.

## 릴리스

맥과 윈도우는 **하나의 릴리스**로 나간다. 같은 `vX.Y.Z` 태그 하나에 맥 dmg와 윈도우 설치
프로그램이 함께 실리고, 버전도 `project.yml`과 `windows/codenotch/Cargo.toml`,
`windows/codenotch/tauri.conf.json` 세 곳이 같아야 한다. `windows-vX.Y.Z` 시리즈는 폐지했다 —
두 플랫폼의 버전이 서로 어긋나고, 설정 화면의 업데이트 확인이 어느 쪽을 봐야 하는지 알 수 없게
만들었다. 남아 있던 `windows-v0.4.x`/`0.5.0`/`0.6.0` 릴리스는 지웠다.

```bash
make release      # Developer ID로 서명하고 notarytool로 공증. 자격증명 필요
make dmg-ci       # 서명 자격증명이 없을 때: ad-hoc 서명 dmg
make publish      # 이미 있는 태그의 릴리스에 dmg 첨부
```

`make release`는 Developer ID 인증서와 `xcrun notarytool store-credentials UsageNotch` 프로필이
둘 다 있어야 한다. 없으면 `make dmg-ci`로 만든 dmg를 쓴다 — 서명도 공증도 없으므로 받는 사람이
Gatekeeper에서 **우클릭 → 열기**를 한 번 해야 하고, 업데이트마다 키체인 허용을 다시 묻는다.

Windows 설치 파일은 현재 자동 첨부되지 않는다. Windows 호스트에서 네이티브 테스트와
NSIS 빌드를 완료한 뒤 검증한 `Provider-Monitor-Setup.exe`를 같은 릴리스에 명시적으로 첨부해야 한다.
설치된 버전과 실행 파일 경로·해시, 사용자 데스크톱 세션, 새 측정값을 함께 확인한다.
아래 게시 명령은 사용자가 커밋·태그·게시를 요청한 경우에만 실행한다:

```bash
VERSION=1.17.1
gh release create "v${VERSION}" "build/ci/ProviderMonitor-${VERSION}-unsigned.dmg" \
  --title "Provider Monitor ${VERSION}" --notes-file notes.md
gh release view "v${VERSION}" --json assets -q '.assets[].name'   # dmg + Setup.exe 둘 다 확인
```

2026-11-17 확인한 원본은 `8405178`(정식 릴리스 1.25.0)이다. 우선 주간-only Claude,
AGY 가족 선택, OpenCode 저장 형식 등의 수정 원리를 선별 적용한다. 우리의 계정별 측정 시각·
마지막 정상값·등록 방식과 Windows 포트를 보존하며, 비용 인덱서나 계정 스위치 등 별도 기능을
안정화 패치와 함께 가져오지 않는다.
