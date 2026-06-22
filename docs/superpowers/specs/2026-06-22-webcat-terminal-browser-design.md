# webcat — 터미널 브라우저 설계 문서

- **날짜**: 2026-06-22
- **상태**: 설계 승인 대기 → 구현 계획 작성 예정
- **작성**: 브레인스토밍 세션 결과

## 1. 개요

webcat은 Chromium을 헤드리스로 구동하고, 그 렌더링 결과를 **kitty 그래픽 프로토콜**로 터미널에 출력하는 Rust 기반 터미널 브라우저다. 마우스와 키보드(vim 스타일 힌트) 양쪽으로 조작하며, **한글을 포함한 비 ASCII 텍스트 입력**을 1급 요구사항으로 다룬다.

### 확정된 핵심 결정

| 항목 | 결정 |
|------|------|
| 목적 | 실사용 데일리 드라이버 도구 |
| 실행 환경 | 로컬 터미널 (로컬 머신에서 터미널 + Chromium 동시 구동) |
| 대상 터미널 | Kitty (그래픽/키보드 프로토콜 레퍼런스 구현) |
| 상호작용 | 마우스 + 키보드(vim 힌트) 둘 다 |
| v1 범위 | 기본 탐색(URL/뒤로·앞으로/새로고침/스크롤) + 링크 클릭/폼 입력(한글) |
| 엔진 통합 | **접근법 A**: CDP + Headless Chromium (`chromiumoxide`) |
| 프로필 | 전용 영구 프로필 (깨끗하게 시작, webcat 안 로그인은 다음 실행에 유지, 평소 Chrome과 완전 분리) |
| 한글 입력 | **완성 글자 단위** — OS IME가 조합한 완성 UTF-8을 `Input.insertText`로 전달 (v1) |

### 범위 밖 (이후 버전)

- 탭 / 멀티 페이지
- 북마크 / 히스토리 / 검색창
- 웹 입력창 내 실시간 IME 조합 표시 (preedit → `Input.imeSetComposition`)
- 평소 Chrome 프로필/쿠키 가져오기
- SSH 원격 실행
- Kitty 외 터미널(Ghostty/WezTerm 등) 지원

### 접근법 선택 근거

- **A (CDP + Headless Chromium)** 채택: Rust 생태계 성숙(`chromiumoxide`), 빠른 v1, `Input.insertText`로 유니코드/한글 완벽 처리, Chrome 업데이트는 OS가 관리. 향후 성능 병목 시 렌더 백엔드만 B로 교체 가능한 구조.
- **B (CEF 임베딩)** 탈락: 빌드 복잡도(수 GB SDK)·Rust 바인딩 미성숙·거대 바이너리, 로컬 환경에선 성능 이점 체감 작음.
- **C (WebView/wry)** 탈락: macOS에서 WebKit(WKWebView)라 "Chromium 기반"이 아니며 헤드리스 픽셀 캡처가 깔끔하지 않음.

## 2. 아키텍처 & 프로세스 모델

```
┌──────────────────────────────────────────────────────────┐
│  webcat (Rust, tokio async)                                │
│                                                            │
│  ┌────────────┐   CDP/WebSocket   ┌──────────────────┐    │
│  │  Browser    │ ◄───────────────► │ Headless Chromium │   │
│  │  Controller │                   │ (별도 프로세스)    │   │
│  └────────────┘                    └──────────────────┘    │
│        ▲  │ frames(JPEG) / input cmds                       │
│        │  ▼                                                 │
│  ┌────────────┐   ┌────────────┐   ┌──────────────┐        │
│  │  App /      │──►│  Renderer   │──►│ Terminal I/O │──► kitty
│  │  Event Loop │   │ (kitty gfx) │   │ (raw mode)   │ ◄── stdin
│  └────────────┘   └────────────┘   └──────────────┘        │
│        ▲                                                    │
│        │ key/mouse events                                  │
│  ┌────────────┐                                            │
│  │ Input Mapper│ (terminal → CDP, vim hints)               │
│  └────────────┘                                            │
└──────────────────────────────────────────────────────────┘
```

### 동작 흐름

1. webcat 시작 → 전용 프로필로 headless Chromium 자식 프로세스 spawn → CDP WebSocket 연결
2. 터미널 크기 + 셀 픽셀 크기 질의 → Chromium 뷰포트를 그 픽셀 크기에 맞춤
3. `Page.startScreencast`로 JPEG 프레임 수신 → kitty 그래픽 프로토콜로 출력
4. stdin에서 키/마우스 이벤트 읽기 → Input Mapper가 CDP 입력 명령으로 변환 → Chromium에 전달
5. 전 과정을 tokio 비동기 이벤트 루프에서 조율

### 프로세스 모델

- webcat은 부모, Chromium은 자식.
- webcat 종료 시 Chromium도 정리(graceful shutdown + 프로세스 kill).
- Chromium이 죽으면 webcat이 감지해서 재시작 또는 에러 표시.

## 3. 컴포넌트 상세

각 모듈은 명확한 단일 책임 + 잘 정의된 인터페이스를 갖고 독립적으로 테스트 가능하게 설계한다.

### 3.1 `browser` — Browser Controller
- **역할**: Chromium 자식 프로세스 생명주기 + 모든 CDP 통신
- **책임**: spawn(전용 프로필 플래그), CDP 연결, 네비게이션(`Page.navigate`, 뒤로/앞으로, 새로고침), 스크린캐스트 시작/정지, 입력 명령 전달(`Input.*`), 뷰포트 크기 설정(`Emulation.setDeviceMetricsOverride`), 크래시 감지·재시작
- **인터페이스(예)**: `navigate(url)`, `go_back()`, `reload()`, `set_viewport(w,h,dpr)`, `dispatch_key(...)`, `insert_text(s)`, `dispatch_mouse(...)`, `frames() -> Stream<Frame>`
- **의존**: `chromiumoxide`, tokio
- 외부에선 CDP를 모르고도 브라우저를 조작 가능

### 3.2 `renderer` — Kitty Graphics Renderer
- **역할**: JPEG 프레임을 받아 kitty 그래픽 프로토콜 이스케이프로 출력
- **책임**: JPEG 패스스루 전송(`f=100`, 필요 시 디코딩 폴백), 이미지 ID 관리, 이전 프레임 in-place 교체(같은 ID 재전송 → 깜빡임 없는 갱신), 셀 그리드에 맞춘 배치, status bar/오버레이 영역 분리
- **인터페이스**: `present(frame: &Frame)`, `clear()`, `resize(cols, rows, cell_px)`
- **의존**: kitty 프로토콜 인코더(직접 구현), base64
- **핵심 결정**: 단일 이미지 placement를 in-place 교체. JPEG를 디코딩 없이 그대로 전송해 CPU 절약

### 3.3 `terminal` — Terminal I/O
- **역할**: 터미널 raw mode 관리, 저수준 입력 바이트 스트림 + 터미널 능력 질의
- **책임**: raw mode 진입/복원, kitty 키보드 프로토콜 활성화(progressive enhancement), SGR 마우스 리포팅 활성화, 셀 픽셀 크기 질의(`CSI 16 t`), 화면 크기/리사이즈(SIGWINCH) 감지, 종료 시 클린업(alt screen 등)
- **인터페이스**: `enable_raw()`, `events() -> Stream<RawInput>`, `cell_size()`, `size()`, 리사이즈 시그널
- **의존**: `crossterm`(raw mode/크기/시그널) + kitty 키보드/그래픽 프로토콜 직접 파싱/인코딩

### 3.4 `input` — Input Mapper
- **역할**: 터미널 raw 입력을 의미 있는 동작 + CDP 입력 명령으로 변환
- **책임**:
  - **텍스트(한글 포함)**: 완성된 UTF-8 → `browser.insert_text()` (`Input.insertText`)
  - **특수키**(Enter, Backspace, 화살표, Tab 등): kitty 키보드 프로토콜로 명확히 식별 → `Input.dispatchKeyEvent`(rawKeyDown/keyUp)
  - **마우스**: SGR 좌표(셀) → 픽셀 변환 → `Input.dispatchMouseEvent`(click/move/wheel)
  - **모드 전환**: 일반 모드 / URL 입력 모드 / vim 힌트 모드
- **인터페이스**: `handle(input: RawInput, mode) -> Action`
- **의존**: browser, 모드 상태

### 3.5 `ui` — Chrome/오버레이
- **역할**: 페이지 위에 그려지는 webcat 자체 UI
- **책임**: 하단 status bar(현재 URL, 로딩 상태), URL 입력 프롬프트(한글 표시 가능), vim 힌트 라벨 오버레이, 에러 토스트
- **인터페이스**: `render_status(...)`, `render_url_prompt(...)`, `render_hints(...)`
- **의존**: terminal(텍스트 출력), renderer와 화면 영역 협의

### 3.6 `app` — Event Loop / Orchestrator
- **역할**: 모든 모듈을 tokio로 묶는 메인 루프
- **책임**: 세 입력 소스(프레임 스트림 / 터미널 입력 / 리사이즈 시그널)를 `select!`로 다중화, 모드 상태 머신 관리, 종료 처리
- **의존**: 위 전부

### vim 힌트 동작
힌트 모드 진입 시 `Runtime.evaluate`로 페이지의 클릭 가능 요소 좌표 수집 → 각 요소에 라벨(a, s, d, f…) 부여 → `ui`가 오버레이로 라벨 표시 → 사용자가 라벨 입력 → 해당 좌표로 `Input.dispatchMouseEvent` 클릭.

## 4. 데이터 흐름 & 성능

### 프레임 파이프라인 (Chromium → 터미널)
```
Page.screencastFrame (CDP 이벤트, base64 JPEG)
  → Browser Controller가 ack(screencastFrameAck) 즉시 전송
  → Frame{ data, metadata } 를 채널에 push
  → App 루프가 수신 → Renderer.present()
  → kitty: 같은 이미지 ID로 재전송하여 in-place 교체
  → 터미널에 표시
```

### 성능 핵심 결정

1. **JPEG 무디코딩 패스스루** — CDP JPEG를 디코딩 없이 kitty `f=100`으로 전송. CPU 절약의 가장 큰 레버. kitty가 거부하는 케이스에만 디코딩 폴백.
2. **이벤트 기반 갱신(고정 FPS 아님)** — 화면이 변할 때만 `screencastFrame` 발생. 정지 페이지에선 CPU·출력이 0에 수렴.
3. **스크린캐스트 파라미터로 부하 제어**: `format: jpeg`, `quality: 60~80`(설정), `maxWidth/maxHeight`(터미널 픽셀 해상도에 맞춤), `everyNthFrame`(부하 시 프레임 솎기).
4. **백프레셔 / 프레임 드롭(coalescing)** — 출력이 생산 속도를 못 따라가면 최신 프레임만 유지하고 중간 프레임 버림. 채널 capacity 1 + 최신값 덮어쓰기로 지연 누적 방지.
5. **입력 지연 최소화** — 입력은 프레임 파이프라인과 독립 경로. 키/마우스는 즉시 CDP 전달. `select!`에서 입력 브랜치 우선.

### 리사이즈 흐름
```
SIGWINCH → terminal.size() 재질의 → 새 픽셀 뷰포트 계산
  → browser.set_viewport(w,h,dpr) → Chromium 리레이아웃
  → renderer.resize() → 다음 프레임부터 새 크기로 표시
```
리사이즈는 디바운스(예: 100ms)로 드래그 중 폭주 방지.

### 해상도 / DPR 매핑
- 터미널 픽셀 크기 = `cols × cell_width_px`, `rows × cell_height_px` (status bar 행 제외)
- 이를 Chromium 뷰포트 픽셀로 설정. DPR 기본 1.0 (고DPR은 선명하지만 프레임 크기·부하 증가 → 설정값으로 노출).

### 예상 체감
정적 페이지는 즉각적, 스크롤은 프레임 드롭으로 지연 없이 부드럽게, 입력 반응은 프레임과 무관하게 빠름.

## 5. 에러 처리 & 엣지 케이스

### 프로세스/연결 장애
- **Chromium 시작 실패**(바이너리 없음/플래그 거부) → 명확한 에러 + 탐색 경로 안내(`/Applications/Google Chrome.app`, `$WEBCAT_CHROME`로 지정 가능). 종료 코드 != 0.
- **Chromium 크래시/CDP 연결 끊김** → 감지 → status bar "재연결 중…" → 자동 재시작(같은 프로필, 마지막 URL 복원). N회 연속 실패 시 포기·알림.
- **프로필 잠김**(이전 인스턴스 비정상 종료) → 잠금 파일 감지 → 안내 후 정리 옵션.

### 터미널 환경 문제
- **kitty 그래픽 미지원 터미널** → 시작 시 capability 탐지(그래픽/키보드 프로토콜 질의 무응답) → 명확히 안내 후 종료. 조용히 깨지지 않게.
- **셀 픽셀 크기 질의 무응답** → 합리적 기본값(예: 8×16) 폴백 + 경고.
- **비정상 종료(panic/SIGTERM/SIGINT)** → 반드시 터미널 복원: raw mode 해제, alt screen 종료, 키보드/마우스 프로토콜 원복, 커서 복원. `Drop` 가드 + 시그널 핸들러 이중 보장.

### 렌더링 엣지
- **프레임 디코딩/전송 실패** → 해당 프레임만 스킵, 다음 프레임 대기(크래시 금지).
- **거대 프레임/느린 출력** → 섹션 4의 frame coalescing으로 흡수.

### 입력 엣지
- **vim 힌트: 클릭 요소 0개** → "클릭 가능한 요소 없음" 토스트 후 모드 해제.
- **포커스 없는 텍스트 입력** → `Input.insertText`는 무시됨. URL 입력 모드가 아니면 일반 키 처리(스크롤 등)로 흘림.
- **한글 조합 도중 모드 전환/ESC** → 터미널이 완성 글자만 주므로 미완성 상태 없음("완성 글자 단위" 결정 덕분).

### 네비게이션 엣지
- **로딩 실패/타임아웃**(DNS, net::ERR_*) → `loadingFailed` 감지 → status bar 에러 표시, 빈 화면 방치 금지.
- **JS 다이얼로그/새 창** → v1 최소 처리: JS 다이얼로그 자동 dismiss 또는 알림, `window.open`은 같은 뷰에서 열기(탭 없음).

### 로깅
에러/진단은 별도 로그 파일(`~/.local/state/webcat/log` 또는 `$WEBCAT_LOG`)에 기록. 터미널 화면은 렌더링에 쓰이므로 stderr 난입 금지.

## 6. 테스트 전략 & 기술 스택

### 기술 스택
- **언어/런타임**: Rust + `tokio`
- **CDP**: `chromiumoxide`
- **터미널**: `crossterm`(raw mode/크기/시그널) + kitty 그래픽/키보드 프로토콜 직접 인코더/파서
- **이미지**: `image`(디코딩 폴백), `base64`
- **에러/로그**: `anyhow`/`thiserror`, `tracing` + 파일 appender
- **CLI**: `clap` (`webcat <url>`, `--profile-dir`, `--chrome`, `--quality`, `--dpr` 등; `--reclone-profile`은 향후)

### 테스트 전략
1. **`renderer` (단위)** — 프레임 + 그리드 크기 → 출력 이스케이프 바이트열 골든 테스트(이미지 ID/배치/in-place 교체 시퀀스). 터미널 없이 순수 함수 테스트.
2. **`input` mapper (단위)** — raw 입력(한글 UTF-8, 특수키 kitty 시퀀스, SGR 마우스) → 기대 `Action`/CDP 명령 매핑. **한글 핵심**: "안녕하세요" → `insert_text("안녕하세요")` 단언. 셀→픽셀 좌표 변환 검증.
3. **kitty 프로토콜 파서 (단위)** — 키보드 프로토콜 이스케이프 → 키 이벤트 파싱(modifier 조합, 멀티바이트 포함).
4. **`browser` controller (통합)** — 실제 headless Chromium + `data:`/로컬 HTML: navigate→프레임 수신, `insert_text`→DOM 값 한글 라운드트립 검증, 클릭→이벤트 확인. CI에서 Chromium 있을 때만(feature flag).
5. **E2E (수동 + 스모크)** — 실제 kitty에서 렌더링/스크롤/링크 클릭/폼 한글 입력 체크리스트. 자동 스모크: 종료 시 터미널 복원 검증.
6. **터미널 복원 (단위/통합)** — 패닉/시그널 시 raw mode·프로토콜·alt screen 원복(`Drop` 가드).

**TDD**: 순수 로직(`renderer`, `input`, 파서)은 테스트 먼저. `browser`/E2E는 통합 테스트.

### 개발 순서 (점진적 동작 확인)
1. terminal + renderer: 정적 이미지 한 장을 kitty로 띄우기
2. browser: headless Chromium 스크린샷 한 장 → 화면 표시
3. 스크린캐스트 연결 → 실시간 프레임
4. 입력: 마우스 클릭 → 스크롤 → 특수키 → **한글 텍스트 입력**
5. ui: status bar + URL 입력 모드 + vim 힌트
6. 에러 처리·복원·폴리시
