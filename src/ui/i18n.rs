use crate::models::settings::UiLanguage;

pub fn t(language: UiLanguage, text: &'static str) -> &'static str {
    if language == UiLanguage::English {
        return text;
    }

    match text {
        "Workspace:" => "워크스페이스:",
        "Input Set" => "입력 세트",
        "Dashboard" => "대시보드",
        "Articles" => "논문",
        "Gather" => "수집",
        "Knowledge Graph" => "지식 그래프",
        "Wiki" => "위키",
        "Gap Bridge" => "갭 브리지",
        "Prompts" => "프롬프트",
        "Settings" => "설정",
        "Traces" => "추적",
        "Interface" => "인터페이스",
        "Language" => "언어",
        "Language updated." => "언어가 변경되었습니다.",
        "Loading settings..." => "설정을 불러오는 중...",
        "Welcome to ResearchWiki" => "ResearchWiki에 오신 것을 환영합니다",
        "Configure the two OpenAI-compatible endpoints ResearchWiki uses." => {
            "ResearchWiki가 사용하는 두 개의 OpenAI 호환 엔드포인트를 설정하세요."
        }
        "You can change either later in Settings." => "나중에 설정에서 변경할 수 있습니다.",
        "Used for evaluation, screening, knowledge-graph extraction, etc." => {
            "평가, 스크리닝, 지식 그래프 추출 등에 사용됩니다."
        }
        "Used to embed article chunks for semantic + hybrid search." => {
            "의미 검색 + 하이브리드 검색을 위해 논문 청크를 임베딩하는 데 사용됩니다."
        }
        "Save and continue" => "저장하고 계속",
        "Ready." => "준비되었습니다.",
        "Restored from system tray." => "시스템 트레이에서 복원되었습니다.",
        "Minimized to system tray. Scheduler remains active." => {
            "시스템 트레이로 최소화되었습니다. 스케줄러는 계속 실행됩니다."
        }

        "Workspace" => "워크스페이스",
        "Create or edit a research workspace. Seed concepts drive gather queries; the gap note feeds Gap Bridge." => {
            "연구 워크스페이스를 만들거나 편집합니다. 시드 개념은 수집 쿼리에, 갭 메모는 갭 브리지에 사용됩니다."
        }
        "Research context" => "연구 맥락",
        "Gather inputs" => "수집 입력",
        "Actions" => "작업",
        "Preview / reference" => "미리보기 / 참고",
        "Create workspace" => "워크스페이스 만들기",
        "Name" => "이름",
        "Primary question" => "주요 질문",
        "Topic descriptor" => "주제 설명",
        "Gap note" => "갭 메모",
        "Seed concepts\n(one per line)" => "시드 개념\n(한 줄에 하나)",
        "Lookback (days)" => "조회 기간(일)",
        "Override queries\n(optional, one per line)" => "대체 쿼리\n(선택, 한 줄에 하나)",
        "Save" => "저장",
        "Save and run gather (all sources)" => "저장 후 수집 실행(전체 소스)",
        "Refined question from Gap Bridge" => "갭 브리지의 정제된 질문",
        "Create" => "만들기",
        "Wiring preview" => "연결 미리보기",
        "Gather search" => "수집 검색",
        "Gather window" => "수집 기간",
        "Screening" => "스크리닝",
        "Fetcher" => "가져오기",
        "KG/wiki" => "KG/위키",
        "Gather caps: each source returns ~50 candidates per query; PMC only looks back 30 days. A long lookback broadens coverage across sources rather than exhaustively." => {
            "수집 제한: 각 소스는 쿼리당 약 50개 후보를 반환하며, PMC는 최근 30일만 조회합니다. 긴 조회 기간은 완전 탐색이 아니라 여러 소스의 범위를 넓히는 데 도움이 됩니다."
        }

        // Setup wizard
        "Step 1 of 2 · Connect" => "2단계 중 1단계 · 연결",
        "Contact email (optional)" => "연락처 이메일(선택)",
        "Sent to scholarly APIs (OpenAlex, Crossref, Unpaywall). Leave blank to skip Unpaywall." => {
            "학술 API(OpenAlex, Crossref, Unpaywall)에 전송됩니다. 비워 두면 Unpaywall을 건너뜁니다."
        }
        "Email" => "이메일",
        "Next" => "다음",
        "(leave blank for local servers)" => "(로컬 서버는 비워 두세요)",
        "Set up your research" => "연구 설정",
        "Step 2 of 2 · Your research" => "2단계 중 2단계 · 연구",
        "Tell ResearchWiki what to gather and study. You can refine this anytime in Input Set." => {
            "ResearchWiki가 무엇을 수집하고 분석할지 알려 주세요. 입력 세트에서 언제든 다시 조정할 수 있습니다."
        }
        "Research name" => "연구 이름",
        "What question are you trying to answer?" => "어떤 질문에 답하려고 하나요?",
        "Key topics & search terms\n(one per line)" => "핵심 주제 및 검색어\n(한 줄에 하나)",
        "Finish setup" => "설정 완료",
        "Skip for now" => "나중에 하기",

        // Input Set (plain-language)
        "Set up what ResearchWiki gathers and studies. These settings drive every gather and the wiki it builds." => {
            "ResearchWiki가 수집하고 분석할 내용을 설정하세요. 이 설정은 모든 수집과 생성되는 위키에 적용됩니다."
        }
        "Research" => "연구",
        "Known gap / what's missing (optional)" => "알려진 갭 / 부족한 부분(선택)",
        "Advanced settings" => "고급 설정",
        "Days to look back" => "조회 기간(일)",
        "Topic descriptor\n(natural-language topic)" => "주제 설명\n(자연어 주제)",
        "used by screening + prompt rewrite" => "스크리닝 + 프롬프트 재작성에 사용",
        "Override search queries\n(optional, one per line)" => {
            "대체 검색 쿼리\n(선택, 한 줄에 하나)"
        }
        "Override queries replace your key topics when searching. Leave blank to use the topics above." => {
            "대체 쿼리는 검색 시 핵심 주제를 대체합니다. 비워 두면 위의 주제를 사용합니다."
        }
        "Save & start gathering" => "저장 후 수집 시작",
        "Save stores this research set. The Gather tab and the daily scheduler both use it to build search queries and prompts — saving alone does not gather." => {
            "저장은 이 연구 세트를 저장합니다. 수집 탭과 일일 스케줄러가 이 내용을 사용해 검색 쿼리와 프롬프트를 구성합니다. 저장만으로는 수집되지 않습니다."
        }
        "Save & start gathering also runs one gather now across all sources, looking back the days set in Advanced settings." => {
            "저장 후 수집 시작은 저장한 뒤, 고급 설정의 조회 기간만큼 전체 소스에서 즉시 한 번 수집합니다."
        }
        "To gather automatically on a schedule, set daily times in Settings → Scheduler." => {
            "정기적으로 자동 수집하려면 설정 → 스케줄러에서 일일 시간을 지정하세요."
        }
        "Create another research set" => "다른 연구 세트 만들기",

        // Gathering schedule (per-research-set cadence)
        "Gathering schedule" => "수집 일정",
        "Optionally gather this research set on a cadence. Checked when you open the app and periodically while it's open (the app must be running or in the tray)." => {
            "이 연구 세트를 주기적으로 수집할 수 있습니다. 앱을 열 때와 실행 중 주기적으로 확인합니다(앱이 실행 중이거나 트레이에 있어야 합니다)."
        }
        "Auto-gather every" => "자동 수집 주기",
        "days" => "일",
        "When due:" => "시점 동작:",
        "Ask me first" => "먼저 묻기",
        "Gather automatically" => "자동으로 수집",
        "Auto-gather looks back far enough to cover the gap since the last run." => {
            "자동 수집은 마지막 실행 이후의 공백을 메울 만큼 조회 기간을 잡습니다."
        }
        "never" => "없음",
        "Last gathered" => "마지막 수집",

        "Run gather" => "수집 실행",
        "Active runs" => "실행 중인 작업",
        "Run history" => "실행 기록",
        "Knowledge graph / wiki backfill" => "지식 그래프 / 위키 백필",
        "Advanced diagnostics" => "고급 진단",
        "Pipeline smoke test" => "파이프라인 스모크 테스트",
        "Scheduler check" => "스케줄러 확인",
        "Source" => "소스",
        "Days back" => "조회 일수",
        "Run gather + KG smoke test" => "수집 + KG 스모크 테스트 실행",
        "KG batch" => "KG 배치",
        "Wiki batch" => "위키 배치",
        "Refresh KG" => "KG 새로고침",
        "Backfill KG batch" => "KG 배치 백필",
        "Compile wiki syntheses" => "위키 합성 컴파일",
        "Run full KG + wiki backfill" => "전체 KG + 위키 백필 실행",
        "Stop full backfill" => "전체 백필 중지",
        "Scheduled source" => "예약 소스",
        "Refresh scheduler" => "스케줄러 새로고침",
        "Run scheduled source now" => "예약 소스 지금 실행",
        "Arm next-minute scheduler test" => "다음 분 스케줄러 테스트 준비",
        "Restore scheduler settings" => "스케줄러 설정 복원",
        "Refresh" => "새로고침",
        "Run all sources" => "전체 소스 실행",
        "No active gather jobs." => "실행 중인 수집 작업이 없습니다.",
        "Details" => "세부 정보",
        "Cancel" => "취소",
        "Requested" => "요청 시각",
        "Status" => "상태",
        "Step" => "단계",
        "Found" => "발견",
        "Saved" => "저장됨",
        "Open" => "열기",

        "Entity search" => "엔티티 검색",
        "Graph view" => "그래프 보기",
        "Search" => "검색",
        "Graph nodes <=" => "그래프 노드 <=",
        "Min degree" => "최소 차수",
        "Type" => "유형",
        "Load graph" => "그래프 불러오기",
        "Zoom" => "확대",
        "Reset view" => "보기 초기화",
        "Search results" => "검색 결과",
        "Neighbors" => "이웃",
        "Click a graph node or a search result to see entity details." => {
            "그래프 노드나 검색 결과를 클릭하면 엔티티 세부 정보를 볼 수 있습니다."
        }
        "No graph data. Adjust the filters and click \"Load graph\", or populate the knowledge graph from the Gather tab (Run gather + KG smoke test)." => {
            "그래프 데이터가 없습니다. 필터를 조정하고 \"그래프 불러오기\"를 클릭하거나 수집 탭에서 지식 그래프를 채우세요(수집 + KG 스모크 테스트 실행)."
        }

        "Search syntheses" => "합성 검색",
        "Filters and compilation" => "필터 및 컴파일",
        "Clear" => "지우기",
        "Stale only" => "오래된 항목만",
        "Apply" => "적용",
        "Compile syntheses" => "합성 컴파일",
        "Select an entity to view its synthesis." => "합성을 보려면 엔티티를 선택하세요.",
        "No syntheses yet. Populate the knowledge graph (Gather tab), then click \"Compile syntheses\". Only entities cited by >=3 articles appear." => {
            "아직 합성이 없습니다. 지식 그래프를 채운 뒤(수집 탭) \"합성 컴파일\"을 클릭하세요. 3개 이상의 논문에서 인용된 엔티티만 표시됩니다."
        }
        "Key aspects" => "핵심 측면",
        "Related entities" => "관련 엔티티",

        "LLM endpoint" => "LLM 엔드포인트",
        "Embedding endpoint" => "임베딩 엔드포인트",
        "Changes are saved to settings.json. Restart to apply to the running LLM client." => {
            "변경 사항은 settings.json에 저장됩니다. 실행 중인 LLM 클라이언트에 적용하려면 다시 시작하세요."
        }
        "Used to embed article chunks for semantic + hybrid search. Restart to apply." => {
            "의미 검색 + 하이브리드 검색을 위해 논문 청크를 임베딩합니다. 적용하려면 다시 시작하세요."
        }
        "Paths" => "경로",
        "Scheduler" => "스케줄러",
        "Embeddings" => "임베딩",
        "Base URL" => "기본 URL",
        "Model" => "모델",
        "API key" => "API 키",
        "Save LLM endpoint" => "LLM 엔드포인트 저장",
        "Save embedding endpoint" => "임베딩 엔드포인트 저장",
        "Contact email" => "연락처 이메일",
        "Sent to scholarly APIs (OpenAlex, Crossref, Unpaywall). Required for Unpaywall; leave blank to skip it. Restart to apply." => {
            "학술 API(OpenAlex, Crossref, Unpaywall)에 전송됩니다. Unpaywall에는 필수이며, 비워 두면 건너뜁니다. 적용하려면 다시 시작하세요."
        }
        "Save contact email" => "연락처 이메일 저장",
        "Semantic Scholar API key" => "Semantic Scholar API 키",
        "Optional. The Semantic Scholar gather source only runs when a key is set (its keyless tier is rate-limited). Get one free at semanticscholar.org. Restart to apply." => {
            "선택 사항. Semantic Scholar 수집 소스는 키가 설정된 경우에만 실행됩니다(키 없는 등급은 요청 제한이 있습니다). semanticscholar.org에서 무료로 발급받을 수 있습니다. 적용하려면 다시 시작하세요."
        }
        "(leave blank to skip Semantic Scholar)" => "(비워 두면 Semantic Scholar를 건너뜁니다)",
        "Save key" => "키 저장",

        // Close-confirmation modal
        "Close ResearchWiki?" => "ResearchWiki를 닫을까요?",
        "Keep it running in the background (system tray), or quit completely?" => {
            "백그라운드(시스템 트레이)에서 계속 실행할까요, 아니면 완전히 종료할까요?"
        }
        "Don't ask again" => "다시 묻지 않기",
        "Minimize to tray" => "트레이로 최소화",
        "Quit" => "종료",

        // Cadence (auto-gather) prompt
        "Gather due" => "수집 시점",
        "This research set is due for a scheduled gather." => {
            "이 연구 세트의 예약 수집 시점입니다."
        }
        "Gather now" => "지금 수집",
        "Not now" => "나중에",
        "Gathering…" => "수집 중…",
        "unsaved changes" => "저장되지 않은 변경 사항",
        "Database" => "데이터베이스",
        "Wiki export" => "위키 내보내기",
        "Settings file" => "설정 파일",
        "Copy" => "복사",
        "Open folder" => "폴더 열기",
        "Enable scheduled gathers" => "예약 수집 사용",
        "Daily schedule (local time, 24h)" => "일일 일정(현지 시간, 24시간)",
        "Hour" => "시",
        "Minute" => "분",
        "Save scheduler" => "스케줄러 저장",
        "Current dimension:" => "현재 차원:",
        "New dimension:" => "새 차원:",
        "Change..." => "변경...",
        "Confirm dimension change" => "차원 변경 확인",
        "Changing the embedding dimension drops the existing vector table on the next startup. All article and entity embeddings will need to be regenerated from scratch." => {
            "임베딩 차원을 변경하면 다음 시작 시 기존 벡터 테이블이 삭제됩니다. 모든 논문 및 엔티티 임베딩을 처음부터 다시 생성해야 합니다."
        }
        "Drop embeddings and save" => "임베딩 삭제 후 저장",

        "Recent articles" => "최근 논문",
        "No articles yet for this workspace." => {
            "이 워크스페이스에는 아직 논문이 없습니다."
        }
        "Filters" => "필터",
        "Category" => "분야",
        "Date from" => "시작일",
        "Date to" => "종료일",
        "Reset" => "초기화",
        "Page size" => "페이지 크기",

        "Broad question" => "넓은 질문",
        "From the broad primary question to the refined, next research question." => {
            "넓은 주요 질문에서 정제된 다음 연구 질문으로 이어집니다."
        }
        "(set the primary question in the Input Set tab)" => {
            "(입력 세트 탭에서 주요 질문을 설정하세요)"
        }
        "Identified gap" => "확인된 갭",
        "(add a gap note in the Input Set tab)" => "(입력 세트 탭에서 갭 메모를 추가하세요)",
        "Refined / next research question" => "정제된 / 다음 연구 질문",
        "Save refined question" => "정제된 질문 저장",
        "Run gap finder (LLM)" => "갭 파인더 실행(LLM)",
        "the focused, answerable trial question that bridges the gap" => {
            "갭을 연결하는 집중적이고 답할 수 있는 시험 질문"
        }
        "\"Run gap finder\" analyzes this workspace's knowledge graph (isolated and under-connected concepts) and asks the LLM to draft the refined question from your primary question + gap note. You can edit and re-save it." => {
            "\"갭 파인더 실행\"은 이 워크스페이스의 지식 그래프(고립되거나 연결이 약한 개념)를 분석하고, 주요 질문 + 갭 메모를 바탕으로 LLM이 정제된 질문 초안을 만들도록 합니다. 이후 편집하고 다시 저장할 수 있습니다."
        }
        "Edit prompt templates (YAML). Saving creates a new version." => {
            "프롬프트 템플릿(YAML)을 편집합니다. 저장하면 새 버전이 생성됩니다."
        }
        "Version history" => "버전 기록",
        "Select a prompt to edit." => "편집할 프롬프트를 선택하세요.",

        // Dashboard
        "Total articles" => "전체 논문",
        "This week" => "이번 주",
        "Evaluated" => "평가 완료",
        "Pending evaluation" => "평가 대기",
        "Articles per day (last 30 days)" => "일별 논문 수(최근 30일)",
        "No articles yet. Open Input Set to describe your research, then run a gather to start building your wiki." => {
            "아직 논문이 없습니다. 입력 세트에서 연구를 설명한 뒤 수집을 실행하면 위키 구축이 시작됩니다."
        }

        // Traces
        "Usage by prompt" => "프롬프트별 사용량",
        "No traces yet — run a gather to populate." => {
            "아직 추적이 없습니다 — 수집을 실행하면 채워집니다."
        }
        "Prompt" => "프롬프트",
        "OK" => "성공",
        "Failed" => "실패",
        "Avg ms" => "평균 ms",
        "Tokens" => "토큰",
        "Cost" => "비용",
        "Result" => "결과",
        "any" => "전체",
        "Loading…" => "불러오는 중…",
        "No traces match these filters." => "이 필터에 해당하는 추적이 없습니다.",
        "Page" => "페이지",
        "traces" => "추적",
        "When" => "시각",
        "Latency" => "지연 시간",
        "Trace detail" => "추적 세부 정보",
        "Article UID" => "논문 UID",
        "Input" => "입력",
        "Output" => "출력",
        "Failed (no error message)" => "실패(오류 메시지 없음)",
        "All" => "전체",

        // Feedback & empty states (UI polish)
        "Retry" => "다시 시도",
        "Saving…" => "저장 중…",
        "No articles yet" => "아직 논문이 없습니다",
        "Run a gather to fetch and evaluate articles for this research set." => {
            "수집을 실행하면 이 연구 세트의 논문을 가져와 평가합니다."
        }
        "No matching articles" => "조건에 맞는 논문이 없습니다",
        "No articles match these filters. Try widening or resetting them." => {
            "이 필터에 해당하는 논문이 없습니다. 조건을 넓히거나 초기화해 보세요."
        }
        "Open Gather" => "수집 탭 열기",
        "Re-extract PDF" => "PDF 다시 추출",
        "Extracting…" => "추출 중…",
        "Runs text extraction again over the stored PDF and refreshes embeddings and the knowledge graph." => {
            "저장된 PDF에서 텍스트 추출을 다시 실행하고 임베딩과 지식 그래프를 갱신합니다."
        }
        "Open Input Set" => "입력 세트 열기",
        "No wiki articles yet" => "아직 위키 문서가 없습니다",
        "Populate the knowledge graph from the Gather tab, then compile \
         syntheses. Only entities cited by >=3 articles appear." => {
            "수집 탭에서 지식 그래프를 채운 뒤 종합을 컴파일하세요. 3개 이상의 논문에 인용된 개체만 표시됩니다."
        }
        "No graph data" => "그래프 데이터가 없습니다",
        "Adjust the filters and click \"Load graph\", or populate the knowledge \
         graph by running a gather first." => {
            "필터를 조정하고 \"그래프 불러오기\"를 클릭하거나, 먼저 수집을 실행해 지식 그래프를 채우세요."
        }
        "No traces yet" => "아직 추적이 없습니다",
        "LLM calls are logged here once a gather or synthesis runs." => {
            "수집이나 종합이 실행되면 LLM 호출이 여기에 기록됩니다."
        }
        "No matching traces" => "조건에 맞는 추적이 없습니다",
        "Clear filters" => "필터 초기화",
        "Articles processed per backfill batch. Larger batches finish faster but use more LLM tokens per run." => {
            "백필 배치당 처리되는 논문 수입니다. 배치가 클수록 빨리 끝나지만 실행당 LLM 토큰을 더 사용합니다."
        }
        "Entities synthesized per compile batch. Larger batches finish faster but use more LLM tokens per run." => {
            "컴파일 배치당 종합되는 개체 수입니다. 배치가 클수록 빨리 끝나지만 실행당 LLM 토큰을 더 사용합니다."
        }
        "How far back gathers search for articles (1–3650 days). Scheduled gathers auto-extend to cover the gap since the last run." => {
            "수집이 논문을 검색하는 기간입니다(1–3650일). 예약 수집은 마지막 실행 이후 공백을 자동으로 보완합니다."
        }
        "1–3650 days" => "1–3650일",
        "Must start with http:// or https://" => "http:// 또는 https://로 시작해야 합니다",
        "(no key set)" => "(키 없음)",
        _ => text,
    }
}
