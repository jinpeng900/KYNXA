use regex::Regex;
use serde_json::{Value, json};

#[derive(Default)]
pub struct DocumentFitState {
    text: Vec<u16>,
    offsets: Vec<usize>,
    index: u64,
    maximum_tokens: u64,
    maximum_body_characters: usize,
    full_tokens: u64,
    stage: String,
    start_index: usize,
    upper_index: usize,
    lower_index: usize,
    search_upper_index: usize,
    middle_index: usize,
    best_index: Option<usize>,
    best_tokens: u64,
    boundary: usize,
    segments: Vec<Value>,
}

impl DocumentFitState {
    fn too_long(&self, tokens: Option<u64>) -> Value {
        let mut details = json!({"index": self.index, "maxInputTokens": self.maximum_tokens});
        if let Some(tokens) = tokens {
            details["tokenCount"] = json!(tokens);
        }
        json!({"errorCode": "EMBEDDING_CONTEXT_TOO_LONG", "details": details})
    }
    fn measure(&self, start: usize, end: usize, context_only: bool) -> Value {
        json!({"start": start, "end": end, "contextOnly": context_only})
    }
    fn preferred_end(&self, start: usize, end: usize) -> usize {
        let minimum = start + (((end - start) as f64) * 0.8).floor() as usize;
        let tail = String::from_utf16_lossy(&self.text[minimum..end]);
        let whitespace = r"[\t-\r \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";
        let boundaries =
            Regex::new(&format!(r"[\n。！？.!?](?:{whitespace}|$)|{whitespace}+")).unwrap();
        boundaries
            .find_iter(&tail)
            .last()
            .map(|matched| minimum + tail[..matched.end()].encode_utf16().count())
            .unwrap_or(end)
    }
    fn segment(&mut self, end: usize) {
        self.segments.push(json!({"start": self.offsets[self.start_index], "end": end, "tokenCount": self.best_tokens}));
        self.start_index = self.best_index.unwrap();
        self.stage = "segment".into();
    }
    pub fn apply(&mut self, operation: &str, args: &Value) -> Result<Value, String> {
        match operation {
            "models.lifecycle.fit.init" => {
                self.text = args["textUtf16"]
                    .as_array()
                    .ok_or("EMBEDDING_INVALID_INPUT")?
                    .iter()
                    .map(|unit| {
                        unit.as_u64()
                            .filter(|unit| *unit <= u16::MAX as u64)
                            .map(|unit| unit as u16)
                            .ok_or("EMBEDDING_INVALID_INPUT".to_owned())
                    })
                    .collect::<Result<_, _>>()?;
                self.index = args["index"].as_u64().unwrap_or(0);
                self.maximum_tokens = args["maxInputTokens"]
                    .as_u64()
                    .ok_or("EMBEDDING_INVALID_INPUT")?;
                self.maximum_body_characters = 16384_usize
                    .saturating_sub(args["contextCharacters"].as_u64().unwrap_or(0) as usize);
                self.offsets = vec![0];
                let mut offset = 0;
                while offset < self.text.len() {
                    offset += if (0xd800..=0xdbff).contains(&self.text[offset])
                        && self
                            .text
                            .get(offset + 1)
                            .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
                    {
                        2
                    } else {
                        1
                    };
                    self.offsets.push(offset);
                }
                self.stage = "full".into();
                Ok(Value::Null)
            }
            "models.lifecycle.fit.next" => loop {
                match self.stage.as_str() {
                    "full" => return Ok(self.measure(0, self.text.len(), false)),
                    "context" => return Ok(self.measure(0, 0, true)),
                    "done" => {
                        return Ok(
                            json!({"done": true, "result": {"tokenCount": self.full_tokens, "segments": self.segments}}),
                        );
                    }
                    "segment" => {
                        if self.start_index == self.offsets.len() - 1 {
                            self.stage = "done".into();
                            continue;
                        }
                        let start = self.offsets[self.start_index];
                        self.upper_index = self.start_index + 1;
                        while self.upper_index + 1 < self.offsets.len()
                            && self.offsets[self.upper_index + 1] - start
                                <= self.maximum_body_characters
                        {
                            self.upper_index += 1;
                        }
                        if self.offsets[self.upper_index] - start > self.maximum_body_characters {
                            return Ok(self.too_long(None));
                        }
                        self.best_index = None;
                        self.stage = "upper".into();
                    }
                    "upper" => {
                        return Ok(self.measure(
                            self.offsets[self.start_index],
                            self.offsets[self.upper_index],
                            false,
                        ));
                    }
                    "search" => {
                        if self.lower_index <= self.search_upper_index {
                            self.middle_index = (self.lower_index + self.search_upper_index) / 2;
                            return Ok(self.measure(
                                self.offsets[self.start_index],
                                self.offsets[self.middle_index],
                                false,
                            ));
                        }
                        self.stage = "boundary".into();
                    }
                    "too-long" => {
                        return Ok(self.measure(
                            self.offsets[self.start_index],
                            self.offsets[self.start_index + 1],
                            false,
                        ));
                    }
                    "boundary" => {
                        let Some(best) = self.best_index else {
                            self.stage = "too-long".into();
                            continue;
                        };
                        let end = self.offsets[best];
                        if best < self.offsets.len() - 1 {
                            let start = self.offsets[self.start_index];
                            self.boundary = self.preferred_end(start, end);
                            if self.boundary > start && self.boundary < end {
                                self.stage = "boundary-measure".into();
                                continue;
                            }
                        }
                        self.segment(end);
                    }
                    "boundary-measure" => {
                        return Ok(self.measure(
                            self.offsets[self.start_index],
                            self.boundary,
                            false,
                        ));
                    }
                    _ => return Err("EMBEDDING_FIT_INVALID_STATE".into()),
                }
            },
            "models.lifecycle.fit.measured" => {
                let tokens = args["tokens"]
                    .as_u64()
                    .ok_or("EMBEDDING_INVALID_TOKEN_COUNT")?;
                match self.stage.as_str() {
                    "full" => {
                        self.full_tokens = tokens;
                        self.stage = "context".into();
                    }
                    "context" => {
                        if self.maximum_body_characters < 1 || tokens >= self.maximum_tokens {
                            return Ok(self.too_long(Some(tokens)));
                        }
                        if self.full_tokens <= self.maximum_tokens
                            && self.text.len() <= self.maximum_body_characters
                        {
                            self.segments.push(json!({"start": 0, "end": self.text.len(), "tokenCount": self.full_tokens}));
                            self.stage = "done".into();
                        } else {
                            self.stage = "segment".into();
                        }
                    }
                    "upper" => {
                        if tokens <= self.maximum_tokens {
                            self.best_index = Some(self.upper_index);
                            self.best_tokens = tokens;
                            self.stage = "boundary".into();
                        } else {
                            self.lower_index = self.start_index + 1;
                            self.search_upper_index = self.upper_index - 1;
                            self.stage = "search".into();
                        }
                    }
                    "search" => {
                        if tokens <= self.maximum_tokens {
                            self.best_index = Some(self.middle_index);
                            self.best_tokens = tokens;
                            self.lower_index = self.middle_index + 1;
                        } else {
                            self.search_upper_index = self.middle_index - 1;
                        }
                    }
                    "too-long" => return Ok(self.too_long(Some(tokens))),
                    "boundary-measure" => {
                        let mut best = self.best_index.unwrap();
                        let end = if tokens <= self.maximum_tokens {
                            self.best_tokens = tokens;
                            while self.offsets[best] > self.boundary {
                                best -= 1;
                            }
                            self.best_index = Some(best);
                            self.boundary
                        } else {
                            self.offsets[best]
                        };
                        self.segment(end);
                    }
                    _ => return Err("EMBEDDING_FIT_INVALID_STATE".into()),
                }
                Ok(Value::Null)
            }
            _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
        }
    }
}
