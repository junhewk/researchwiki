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
        "unsaved changes" => "저장되지 않은 변경 사항",
        "Database" => "데이터베이스",
        "Wiki export" => "위키 내보내기",
        "Settings file" => "설정 파일",
        "Copy" => "복사",
        "Open folder" => "폴더 열기",
        "Enable scheduled gathers" => "예약 수집 사용",
        "Daily schedule (KST, 24h)" => "일일 일정(KST, 24시간)",
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

        "Top articles" => "상위 논문",
        "No scored articles yet for this workspace." => {
            "이 워크스페이스에는 아직 점수가 매겨진 논문이 없습니다."
        }
        "Filters" => "필터",
        "Category" => "분야",
        "Date from" => "시작일",
        "Date to" => "종료일",
        "Min score" => "최소 점수",
        "Max score" => "최대 점수",
        "Tier" => "티어",
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
        _ => text,
    }
}
