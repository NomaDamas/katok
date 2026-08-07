#!/usr/bin/env python3
"""Generate a synthetic KakaoTalk fixture for demos.

Everything here is invented. No line comes from a real archive.

The dataset is shaped for one demo question -- "이번 달에 내가 하기로 약속한 것들,
방 상관없이 다 모아줘" -- so the search has to do real work to answer it:

- commitments live in 5 different rooms, so a single-room read misses them
- other people also make commitments, so the answer needs a sender filter
- July commitments sit next to August ones, so the answer needs a date filter
- small talk outnumbers commitments, so a keyword sweep has to reject noise

Usage:
    python3 scripts/generate_demo_fixture.py [out.jsonl]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

ACCOUNT = "acct-demo"
OWNER = "김하늘"  # the demo account owner

# sender_id is stable per persona; katok resolves display names, not ids.
PEOPLE = {
    OWNER: "u-100",
    "최현우": "u-201",
    "한지민": "u-202",
    "오세훈": "u-203",
    "배수정": "u-204",
    "박지훈": "u-205",
    "이서연": "u-206",
    "정민우": "u-207",
    "윤가람": "u-208",
    "노아린": "u-209",
}

ROOMS = {
    "delivery": ("2001", "러닝메이트 강의 납기", "group"),
    "class": ("2002", "AI 클래스 제작팀", "group"),
    "study": ("2003", "사내 AI 스터디", "group"),
    "crew": ("2004", "동네 러닝크루", "group"),
    "jihoon": ("3001", "박지훈", "direct"),
    "seoyeon": ("3002", "이서연님", "direct"),
    "minwoo": ("3003", "정민우", "direct"),
    "garam": ("3004", "윤가람", "direct"),
}

# (room key, "YYYY-MM-DD HH:MM" in UTC, sender, text)
# KST = UTC + 9h. 01:00Z reads as 10:00 in the morning for a Korean demo.
SCRIPT: list[tuple[str, str, str, str]] = [
    # --- July: commitments that must NOT show up in a "this month" answer ---
    (
        "delivery",
        "2026-07-21 01:10",
        "최현우",
        "하늘님 1차 커리큘럼 초안 언제쯤 볼 수 있을까요?",
    ),
    (
        "delivery",
        "2026-07-21 01:24",
        OWNER,
        "이번 주 금요일까지 초안 정리해서 공유드리겠습니다.",
    ),
    ("delivery", "2026-07-24 08:02", OWNER, "초안 올려뒀습니다. 확인 부탁드려요!"),
    ("delivery", "2026-07-24 08:40", "최현우", "확인했습니다. 고생하셨어요"),
    (
        "seoyeon",
        "2026-07-28 05:15",
        "이서연",
        "지난번 말씀하신 리서치 자료 아직 있으실까요?",
    ),
    ("seoyeon", "2026-07-28 05:31", OWNER, "네 찾아서 이번 주 안에 보내드릴게요."),
    ("study", "2026-07-29 10:02", "노아린", "다음 스터디 주제 뭐로 할까요?"),
    ("study", "2026-07-29 10:20", "윤가람", "제가 RAG 파트 준비해올게요"),
    ("crew", "2026-07-30 12:00", "박지훈", "토요일 아침 러닝 가시는 분?"),
    ("crew", "2026-07-30 12:04", OWNER, "저요! 7시에 갈게요"),
    # --- August: the answer set ---
    ("seoyeon", "2026-08-01 02:12", "이서연", "하늘님 자료 혹시 정리되셨을까요?"),
    (
        "seoyeon",
        "2026-08-01 02:40",
        OWNER,
        "죄송해요 늦었습니다. 오늘 중으로 정리해서 보낼게요.",
    ),
    ("seoyeon", "2026-08-01 02:41", OWNER, "표까지 같이 만들어서 드릴게요!"),
    ("crew", "2026-08-01 09:30", "한지민", "주말 러닝 코스 바뀌었대요"),
    ("crew", "2026-08-01 09:33", OWNER, "오 알겠습니다"),
    ("delivery", "2026-08-02 00:55", "최현우", "이번 주 업로드 일정 공유 가능할까요?"),
    (
        "delivery",
        "2026-08-02 01:20",
        OWNER,
        "안녕하세요.\n2-1부터 2-3까지 촬영은 끝났고 편집 중입니다.\n"
        "2-3까지 편집 마무리해서 내일 오전까지 업로드하겠습니다.",
    ),
    ("delivery", "2026-08-02 02:05", "한지민", "확인했습니다!"),
    (
        "delivery",
        "2026-08-03 01:15",
        OWNER,
        "어제 말씀드린 일정을 못 지켰습니다. 죄송합니다.\n"
        "2-3까지는 올려뒀고, 남은 파트는 오늘 자정 전까지 전체 전달드리겠습니다.",
    ),
    ("delivery", "2026-08-03 01:40", "최현우", "넵 기다리겠습니다"),
    ("study", "2026-08-03 11:02", "윤가람", "이번 주 스터디 자료 제가 올릴게요"),
    ("study", "2026-08-03 11:10", OWNER, "감사합니다 가람님"),
    (
        "delivery",
        "2026-08-04 02:30",
        OWNER,
        "일정이 또 밀렸습니다. 정말 죄송합니다.\n"
        "오늘 오후 6시까지 반드시 마무리해서 전달드리겠습니다.",
    ),
    ("delivery", "2026-08-04 07:55", "최현우", "혹시 진행 상황 공유 가능하실까요?"),
    ("minwoo", "2026-08-04 09:12", "정민우", "형 이력서 한 번만 봐줄 수 있어?"),
    ("minwoo", "2026-08-04 09:40", OWNER, "당연하지. 주말에 정리해서 피드백 드릴게요"),
    ("jihoon", "2026-08-05 04:02", "박지훈", "하늘님 다음 주에 시간 되세요?"),
    (
        "jihoon",
        "2026-08-05 04:20",
        OWNER,
        "네 좋아요! 다음 주 화요일로 커피챗 잡을게요.",
    ),
    ("class", "2026-08-05 06:10", "오세훈", "실습 코드 레포는 어디서 받나요?"),
    (
        "class",
        "2026-08-05 06:35",
        OWNER,
        "아직 정리 중입니다. 금요일까지 정리해서 공유드리겠습니다.",
    ),
    ("class", "2026-08-05 06:38", "배수정", "넵 감사합니다"),
    (
        "delivery",
        "2026-08-06 01:05",
        OWNER,
        "현재 2-5까지 마무리했고 2-6은 편집 중입니다.\n"
        "오늘 오후 8시까지 꼭 전체 영상 전달드리겠습니다. 계속 늦어져 죄송합니다.",
    ),
    ("delivery", "2026-08-06 03:22", "최현우", "확인했습니다"),
    ("garam", "2026-08-06 05:40", "윤가람", "발표 자료 제가 만들어둘게요"),
    ("garam", "2026-08-06 05:44", OWNER, "오 감사합니다"),
    ("study", "2026-08-06 10:15", OWNER, "다음 주 스터디는 참석 어려울 것 같습니다"),
    (
        "delivery",
        "2026-08-07 00:50",
        OWNER,
        "영상 전달드립니다. 드라이브에 전체 업로드해뒀습니다.\n"
        "누락되거나 수정 필요한 부분 있으면 바로 수정하겠습니다.\n"
        "보너스 강의 1편은 오늘 내 전달이 어려울 것 같습니다. "
        "주말 동안 마무리해서 월요일 오전까지 반드시 전달드리겠습니다.",
    ),
    ("delivery", "2026-08-07 01:30", "최현우", "고생 많으셨습니다!"),
    (
        "class",
        "2026-08-07 08:20",
        OWNER,
        "안녕하세요!\n예상보다 일정이 밀려 아직 촬영을 마무리하지 못했습니다.\n"
        "주말 동안 촬영해서 월요일까지 전달드리겠습니다.",
    ),
    ("class", "2026-08-07 08:44", "배수정", "넵 확인했습니다"),
    ("crew", "2026-08-07 11:00", "박지훈", "이번 주말 러닝 쉬어요"),
    ("crew", "2026-08-07 11:02", OWNER, "넵 알겠습니다 ㅋㅋ"),
]


def build() -> list[dict]:
    messages = []
    for index, (room_key, stamp, sender, text) in enumerate(SCRIPT, start=1):
        chat_id, chat_name, chat_type = ROOMS[room_key]
        messages.append(
            {
                "account_hash": ACCOUNT,
                "chat_id": chat_id,
                "chat_name": chat_name,
                "chat_type": chat_type,
                "message_id": f"demo-{index:04d}",
                "sender_id": PEOPLE[sender],
                "sender_nickname": sender,
                "timestamp": f"{stamp.replace(' ', 'T')}:00Z",
                "text": text,
                "message_type": "text",
                "reply_to_message_id": None,
            }
        )
    return messages


def main() -> int:
    out = (
        Path(sys.argv[1])
        if len(sys.argv) > 1
        else Path("fixtures/demo/demo_messages.jsonl")
    )
    out.parent.mkdir(parents=True, exist_ok=True)
    messages = build()
    with out.open("w", encoding="utf-8") as handle:
        for message in messages:
            handle.write(json.dumps(message, ensure_ascii=False) + "\n")
    owner_august = sum(
        1
        for m in messages
        if m["sender_nickname"] == OWNER and m["timestamp"] >= "2026-07-31T15:00:00Z"
    )
    print(
        f"wrote {len(messages)} messages to {out} (owner={OWNER}, owner August msgs={owner_august})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
