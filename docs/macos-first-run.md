# macOS 첫 설정

`katok`은 카카오톡 준비 상태를 자동으로 확인할 수 있지만, macOS는 CLI가 스스로 전체 디스크 접근 권한이나 손쉬운 사용 권한을 부여하는 것을 허용하지 않습니다. 설정 helper는 대신 아래 순서로 안내합니다.

1. `katok permissions macos`로 필요한 시스템 설정 화면을 엽니다.
2. 현재 터미널 앱을 허용하도록 안내합니다.
3. `katok doctor --macos-probe --json`으로 카카오톡 앱, 컨테이너, DB 접근 가능 여부를 확인합니다.
4. `katok sync --source macos --json`으로 로컬 아카이브를 만듭니다.
5. `katok index --json`으로 EmbeddingGemma 벡터 인덱스를 만듭니다.
6. semantic smoke search를 실행합니다.

helper 실행:

```bash
scripts/katok-macos-setup.sh
```

직접 빌드한 binary를 사용할 때:

```bash
KATOK_BIN=target/debug/katok scripts/katok-macos-setup.sh
```

`katok: command not found`가 나오면 먼저 설치와 PATH를 확인합니다.

```bash
cargo install katok
export PATH="$HOME/.cargo/bin:$PATH"
katok permissions macos --accessibility
```

권한 설정 화면만 직접 열려면:

```bash
katok permissions macos
```

KakaoTalk UI 자동화까지 쓸 계획이면 손쉬운 사용 설정도 같이 엽니다.

```bash
katok permissions macos --accessibility
```

## 완전 자동화가 안 되는 이유

Apple TCC 권한 시스템은 전체 디스크 접근 권한과 손쉬운 사용 권한에 대해 사용자의 시스템 설정 조작을 요구합니다. 로컬 앱은 설정 화면을 열고 결과를 확인할 수 있지만, 자기 자신에게 권한을 몰래 부여할 수는 없습니다.

## 기대 결과

권한을 켠 뒤 `katok doctor --macos-probe --json`은 대략 아래 상태를 보여야 합니다.

```json
{
  "source_adapter": {
    "macos": {
      "status": "checked",
      "app_installed": true,
      "container_present": true,
      "db_file_count": 1
    }
  }
}
```

정확한 `db_file_count`는 카카오톡 계정 상태와 로컬 동기화 상태에 따라 달라질 수 있습니다.
