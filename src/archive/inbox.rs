use super::Archive;
use crate::{
    types::{MentionInboxCounts, MentionInboxItem, MentionInboxReport, MentionStatus},
    Error, Result,
};
use rusqlite::{params, OptionalExtension};

struct MentionRow {
    account_hash: String,
    chat_id: String,
    chat_name: String,
    message_id: String,
    sender_nickname: String,
    timestamp: String,
    text: String,
    chunk_id: Option<String>,
}

struct SelfResponse {
    message_id: String,
    timestamp: String,
    text: String,
    chunk_id: Option<String>,
}

impl Archive {
    /// Build a read-only reply queue from explicit Kakao mention metadata.
    ///
    /// A direct self-authored reply is definitive. A later self-authored
    /// message without a reply edge is deliberately labelled `review` rather
    /// than guessed answered: it may be unrelated conversation in a busy room.
    pub fn mention_inbox(
        &self,
        since: &str,
        chat_id: Option<&str>,
        include_answered: bool,
        limit: usize,
        snippet_chars: usize,
    ) -> Result<MentionInboxReport> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT m.account_hash, m.chat_id, m.chat_name, m.message_id,
                        m.sender_nickname, m.timestamp, m.text,
                        (SELECT cm.chunk_id
                         FROM chunk_messages cm
                         JOIN chunks c ON c.chunk_id = cm.chunk_id
                         WHERE cm.message_id = m.message_id
                           AND c.account_hash = m.account_hash
                           AND c.chat_id = m.chat_id
                         ORDER BY c.started_at, c.chunk_id
                         LIMIT 1) AS chunk_id
                 FROM messages m
                 WHERE m.mentions_self = 1 AND m.is_self = 0
                   AND m.timestamp >= ?1
                   AND (?2 IS NULL OR m.chat_id = ?2)
                 ORDER BY m.timestamp DESC, m.message_id DESC",
            )
            .map_err(Error::Sql)?;
        let mentions = stmt
            .query_map(params![since, chat_id], |row| {
                Ok(MentionRow {
                    account_hash: row.get(0)?,
                    chat_id: row.get(1)?,
                    chat_name: row.get(2)?,
                    message_id: row.get(3)?,
                    sender_nickname: row.get(4)?,
                    timestamp: row.get(5)?,
                    text: row.get(6)?,
                    chunk_id: row.get(7)?,
                })
            })
            .map_err(Error::Sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::Sql)?;

        let mut counts = MentionInboxCounts::default();
        let mut items = Vec::new();
        for mention in mentions {
            let direct = self.first_self_response(&mention, true)?;
            let later = if direct.is_none() {
                self.first_self_response(&mention, false)?
            } else {
                None
            };
            let (status, response) = if let Some(response) = direct {
                (MentionStatus::Answered, Some(response))
            } else if let Some(response) = later {
                (MentionStatus::Review, Some(response))
            } else {
                (MentionStatus::Pending, None)
            };

            match status {
                MentionStatus::Pending => counts.pending += 1,
                MentionStatus::Review => counts.review += 1,
                MentionStatus::Answered => counts.answered += 1,
            }
            if status == MentionStatus::Answered && !include_answered {
                continue;
            }
            if items.len() >= limit {
                continue;
            }
            let (response_message_id, response_timestamp, response_snippet, response_chunk_id) =
                response
                    .map(|response| {
                        (
                            Some(response.message_id),
                            Some(response.timestamp),
                            Some(snippet(&response.text, snippet_chars)),
                            response.chunk_id,
                        )
                    })
                    .unwrap_or((None, None, None, None));
            items.push(MentionInboxItem {
                status,
                chat_id: mention.chat_id,
                chat_name: mention.chat_name,
                message_id: mention.message_id,
                sender_nickname: mention.sender_nickname,
                timestamp: mention.timestamp,
                snippet: snippet(&mention.text, snippet_chars),
                chunk_id: mention.chunk_id,
                response_message_id,
                response_timestamp,
                response_snippet,
                response_chunk_id,
            });
        }

        Ok(MentionInboxReport {
            since: since.to_string(),
            chat_id: chat_id.map(str::to_string),
            counts,
            returned: items.len(),
            items,
        })
    }

    fn first_self_response(
        &self,
        mention: &MentionRow,
        direct_only: bool,
    ) -> Result<Option<SelfResponse>> {
        let direct_clause = if direct_only {
            "AND reply_to_message_id = ?6"
        } else {
            "AND reply_to_message_id IS NOT ?6"
        };
        let sql = format!(
            "SELECT self_msg.message_id, self_msg.timestamp, self_msg.text,
                    (SELECT cm.chunk_id
                     FROM chunk_messages cm
                     JOIN chunks c ON c.chunk_id = cm.chunk_id
                     WHERE cm.message_id = self_msg.message_id
                       AND c.account_hash = self_msg.account_hash
                       AND c.chat_id = self_msg.chat_id
                     ORDER BY c.started_at, c.chunk_id
                     LIMIT 1) AS chunk_id
             FROM messages self_msg
             WHERE self_msg.account_hash = ?1 AND self_msg.chat_id = ?2
               AND self_msg.is_self = 1
               AND (self_msg.timestamp > ?3 OR
                    (self_msg.timestamp = ?3 AND self_msg.message_id > ?4))
               {direct_clause}
             ORDER BY self_msg.timestamp, self_msg.message_id
             LIMIT ?5"
        );
        self.conn
            .query_row(
                &sql,
                params![
                    mention.account_hash,
                    mention.chat_id,
                    mention.timestamp,
                    mention.message_id,
                    1_i64,
                    mention.message_id
                ],
                |row| {
                    Ok(SelfResponse {
                        message_id: row.get(0)?,
                        timestamp: row.get(1)?,
                        text: row.get(2)?,
                        chunk_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Error::Sql)
    }
}

fn snippet(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut shortened = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    shortened.push('…');
    shortened
}

#[cfg(test)]
mod tests {
    use super::snippet;

    #[test]
    fn snippet_is_unicode_safe_and_collapses_whitespace() {
        assert_eq!(snippet("검토\n  부탁드립니다", 8), "검토 부탁드립…");
        assert_eq!(snippet("짧음", 10), "짧음");
        assert_eq!(snippet("내용", 0), "");
    }
}
