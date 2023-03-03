# futaba-bot

으어어 좀비가 몰려있는 디코 서버를 위한 봇

## 개발

### 요구 사항

- rust >= 1.50

### 작업공간 초기화 방법

이 프로젝트는 sqlx를 사용하며 컴파일타임 SQL타입체크를 하고 있어서 빌드 전에 DB를 미리 초기화 해야합니다.
이 과정을 편하게 하기 위해서 sqlx-cli를 cargo를 통해서 설치합니다.

```bash
cargo install sqlx-cli
```

다음 명령을 통해서 최신 DB스키마로 초기화된 sqlite db파일(db.db)를 생성할 수 있습니다.
```bash
sqlx database create
sqlx migrate run
```

DB를 초기화 하고 싶으면 단순히 db.db를 삭제하고 위 명령을 다시 실행하면 됩니다.


## 실행

다음 권한이 봇에게 있어야 합니다.
- `Read Messages/View Channels`: 메시지가 으어어인지 확인용
- `Manage Message`: 으어어가 아닌 메시지 삭제용
- `Read Message History`: 과거의 으어어 기록 확인용

다음 환경변수를 적당한 값으로 설정해야합니다.
- `DISCORD_BOT_TOKEN`
- `GUILD_ID`
- `EUEOEO_CHANNEL_ID`: 으어어 채널 ID
- `APPLICATION_ID` 
- `EUEOEO_INIT_MESSAGE_ID`: 으어어를 카운트 시작할 메시지 ID(미포함)
