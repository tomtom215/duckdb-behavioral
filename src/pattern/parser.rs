// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Recursive descent parser for sequence match pattern strings.
//!
//! Parses patterns like `(?1).*(?2)(?t>=3600)(?3)` into a structured AST
//! that can be executed by the NFA engine.

use std::fmt;

/// A single step in a compiled pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternStep {
    /// Match an event where condition N (0-indexed internally) is true.
    Condition(usize),
    /// Match zero or more events (any conditions). Corresponds to `.*`.
    AnyEvents,
    /// Match exactly one event (any conditions). Corresponds to `.`.
    OneEvent,
    /// Time constraint relative to the previous matched event.
    /// The duration is in seconds (matching `ClickHouse` semantics).
    TimeConstraint(TimeOp, i64),
}

/// Comparison operator for time constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeOp {
    /// `>=`
    Gte,
    /// `<=`
    Lte,
    /// `>`
    Gt,
    /// `<`
    Lt,
    /// `==`
    Eq,
    /// `!=`
    Ne,
}

impl TimeOp {
    /// Evaluates the time constraint: `elapsed_seconds <op> threshold`.
    #[must_use]
    pub const fn evaluate(self, elapsed_seconds: i64, threshold: i64) -> bool {
        match self {
            Self::Gte => elapsed_seconds >= threshold,
            Self::Lte => elapsed_seconds <= threshold,
            Self::Gt => elapsed_seconds > threshold,
            Self::Lt => elapsed_seconds < threshold,
            Self::Eq => elapsed_seconds == threshold,
            Self::Ne => elapsed_seconds != threshold,
        }
    }
}

/// A compiled pattern ready for execution.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CompiledPattern {
    /// Ordered steps that events must match.
    pub steps: Vec<PatternStep>,
}

/// Error returned when pattern parsing fails.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PatternError {
    /// Human-readable error message.
    pub message: String,
    /// Position in the input string where the error occurred.
    pub position: usize,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pattern error at position {}: {}",
            self.position, self.message
        )
    }
}

impl std::error::Error for PatternError {}

/// Parses a pattern string into a [`CompiledPattern`].
///
/// # Errors
///
/// Returns [`PatternError`] if the pattern string is malformed.
///
/// # Examples
///
/// ```
/// use behavioral::pattern::parser::parse_pattern;
///
/// let pattern = parse_pattern("(?1).*(?2)").unwrap();
/// assert_eq!(pattern.steps.len(), 3);
/// ```
pub fn parse_pattern(input: &str) -> Result<CompiledPattern, PatternError> {
    let mut parser = Parser::new(input);
    let steps = parser.parse()?;
    if steps.is_empty() {
        return Err(PatternError {
            message: "empty pattern".to_string(),
            position: 0,
        });
    }
    Ok(CompiledPattern { steps })
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    const fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn parse(&mut self) -> Result<Vec<PatternStep>, PatternError> {
        let mut steps = Vec::new();
        while self.pos < self.input.len() {
            self.skip_whitespace();
            if self.pos >= self.input.len() {
                break;
            }
            let step = self.parse_step()?;
            steps.push(step);
        }
        Ok(steps)
    }

    fn parse_step(&mut self) -> Result<PatternStep, PatternError> {
        match self.peek() {
            Some(b'(') => self.parse_group(),
            Some(b'.') => self.parse_dot(),
            Some(c) => Err(PatternError {
                message: format!("unexpected character '{}'", char::from(c)),
                position: self.pos,
            }),
            None => Err(PatternError {
                message: "unexpected end of pattern".to_string(),
                position: self.pos,
            }),
        }
    }

    fn parse_group(&mut self) -> Result<PatternStep, PatternError> {
        self.expect(b'(')?;
        self.expect(b'?')?;

        match self.peek() {
            Some(b't') => self.parse_time_constraint(),
            Some(c) if c.is_ascii_digit() => self.parse_condition(),
            Some(c) => Err(PatternError {
                message: format!("expected digit or 't' after '(?', got '{}'", char::from(c)),
                position: self.pos,
            }),
            None => Err(PatternError {
                message: "unexpected end of pattern after '(?'".to_string(),
                position: self.pos,
            }),
        }
    }

    fn parse_condition(&mut self) -> Result<PatternStep, PatternError> {
        let start = self.pos;
        let num = self.parse_number()?;
        self.expect(b')')?;
        if num == 0 {
            return Err(PatternError {
                message: "condition index must be >= 1 (1-indexed)".to_string(),
                position: start,
            });
        }
        // Convert from 1-indexed (user-facing) to 0-indexed (internal)
        Ok(PatternStep::Condition(num - 1))
    }

    fn parse_time_constraint(&mut self) -> Result<PatternStep, PatternError> {
        self.expect(b't')?;
        let op = self.parse_time_op()?;
        let seconds = self.parse_number()? as i64;
        self.expect(b')')?;
        Ok(PatternStep::TimeConstraint(op, seconds))
    }

    fn parse_time_op(&mut self) -> Result<TimeOp, PatternError> {
        match (self.peek(), self.peek_at(1)) {
            (Some(b'>'), Some(b'=')) => {
                self.advance();
                self.advance();
                Ok(TimeOp::Gte)
            }
            (Some(b'<'), Some(b'=')) => {
                self.advance();
                self.advance();
                Ok(TimeOp::Lte)
            }
            (Some(b'='), Some(b'=')) => {
                self.advance();
                self.advance();
                Ok(TimeOp::Eq)
            }
            (Some(b'!'), Some(b'=')) => {
                self.advance();
                self.advance();
                Ok(TimeOp::Ne)
            }
            (Some(b'>'), _) => {
                self.advance();
                Ok(TimeOp::Gt)
            }
            (Some(b'<'), _) => {
                self.advance();
                Ok(TimeOp::Lt)
            }
            _ => Err(PatternError {
                message: "expected comparison operator (>=, <=, >, <, ==, !=) after '(?t'"
                    .to_string(),
                position: self.pos,
            }),
        }
    }

    fn parse_dot(&mut self) -> Result<PatternStep, PatternError> {
        self.expect(b'.')?;
        if self.peek() == Some(b'*') {
            self.advance();
            Ok(PatternStep::AnyEvents)
        } else {
            Ok(PatternStep::OneEvent)
        }
    }

    fn parse_number(&mut self) -> Result<usize, PatternError> {
        let start = self.pos;
        let mut num: usize = 0;
        let mut digits = 0;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                num = num
                    .checked_mul(10)
                    .and_then(|n| n.checked_add((c - b'0') as usize))
                    .ok_or_else(|| PatternError {
                        message: "number overflow in pattern".to_string(),
                        position: start,
                    })?;
                digits += 1;
                self.advance();
            } else {
                break;
            }
        }
        if digits == 0 {
            return Err(PatternError {
                message: "expected number".to_string(),
                position: self.pos,
            });
        }
        Ok(num)
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.input.get(self.pos + offset).copied()
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn expect(&mut self, expected: u8) -> Result<(), PatternError> {
        match self.peek() {
            Some(c) if c == expected => {
                self.advance();
                Ok(())
            }
            Some(c) => Err(PatternError {
                message: format!(
                    "expected '{}', got '{}'",
                    char::from(expected),
                    char::from(c)
                ),
                position: self.pos,
            }),
            None => Err(PatternError {
                message: format!("expected '{}', got end of pattern", char::from(expected)),
                position: self.pos,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_condition() {
        let p = parse_pattern("(?1)").unwrap();
        assert_eq!(p.steps, vec![PatternStep::Condition(0)]);
    }

    #[test]
    fn test_two_conditions() {
        let p = parse_pattern("(?1)(?2)").unwrap();
        assert_eq!(
            p.steps,
            vec![PatternStep::Condition(0), PatternStep::Condition(1)]
        );
    }

    #[test]
    fn test_wildcard_between() {
        let p = parse_pattern("(?1).*(?2)").unwrap();
        assert_eq!(
            p.steps,
            vec![
                PatternStep::Condition(0),
                PatternStep::AnyEvents,
                PatternStep::Condition(1),
            ]
        );
    }

    #[test]
    fn test_single_event_between() {
        let p = parse_pattern("(?1).(?2)").unwrap();
        assert_eq!(
            p.steps,
            vec![
                PatternStep::Condition(0),
                PatternStep::OneEvent,
                PatternStep::Condition(1),
            ]
        );
    }

    #[test]
    fn test_time_constraint() {
        let p = parse_pattern("(?1)(?t>=3600)(?2)").unwrap();
        assert_eq!(
            p.steps,
            vec![
                PatternStep::Condition(0),
                PatternStep::TimeConstraint(TimeOp::Gte, 3600),
                PatternStep::Condition(1),
            ]
        );
    }

    #[test]
    fn test_all_time_ops() {
        for (op_str, op) in &[
            (">=", TimeOp::Gte),
            ("<=", TimeOp::Lte),
            (">", TimeOp::Gt),
            ("<", TimeOp::Lt),
            ("==", TimeOp::Eq),
            ("!=", TimeOp::Ne),
        ] {
            let pat = format!("(?1)(?t{op_str}100)(?2)");
            let p = parse_pattern(&pat).unwrap();
            assert_eq!(
                p.steps[1],
                PatternStep::TimeConstraint(*op, 100),
                "failed for operator {op_str}"
            );
        }
    }

    #[test]
    fn test_complex_pattern() {
        let p = parse_pattern("(?1).*(?2).*(?3)(?4)").unwrap();
        assert_eq!(p.steps.len(), 6);
        assert_eq!(p.steps[0], PatternStep::Condition(0));
        assert_eq!(p.steps[1], PatternStep::AnyEvents);
        assert_eq!(p.steps[2], PatternStep::Condition(1));
        assert_eq!(p.steps[3], PatternStep::AnyEvents);
        assert_eq!(p.steps[4], PatternStep::Condition(2));
        assert_eq!(p.steps[5], PatternStep::Condition(3));
    }

    #[test]
    fn test_condition_zero_rejected() {
        let err = parse_pattern("(?0)").unwrap_err();
        assert!(err.message.contains("must be >= 1"));
    }

    #[test]
    fn test_empty_pattern_rejected() {
        let err = parse_pattern("").unwrap_err();
        assert!(err.message.contains("empty"));
    }

    #[test]
    fn test_whitespace_tolerance() {
        let p = parse_pattern(" (?1) .* (?2) ").unwrap();
        assert_eq!(p.steps.len(), 3);
    }

    #[test]
    fn test_invalid_character() {
        let err = parse_pattern("(?1)x(?2)").unwrap_err();
        assert!(err.message.contains("unexpected character"));
    }

    #[test]
    fn test_unclosed_group() {
        let err = parse_pattern("(?1").unwrap_err();
        assert!(err.message.contains("expected ')'"));
    }

    #[test]
    fn test_multi_digit_condition() {
        let p = parse_pattern("(?12)").unwrap();
        assert_eq!(p.steps, vec![PatternStep::Condition(11)]); // 12 -> 0-indexed 11
    }

    #[test]
    fn test_time_op_evaluate() {
        assert!(TimeOp::Gte.evaluate(10, 10));
        assert!(TimeOp::Gte.evaluate(11, 10));
        assert!(!TimeOp::Gte.evaluate(9, 10));

        assert!(TimeOp::Lte.evaluate(10, 10));
        assert!(TimeOp::Lte.evaluate(9, 10));
        assert!(!TimeOp::Lte.evaluate(11, 10));

        assert!(TimeOp::Gt.evaluate(11, 10));
        assert!(!TimeOp::Gt.evaluate(10, 10));

        assert!(TimeOp::Lt.evaluate(9, 10));
        assert!(!TimeOp::Lt.evaluate(10, 10));

        assert!(TimeOp::Eq.evaluate(10, 10));
        assert!(!TimeOp::Eq.evaluate(11, 10));

        assert!(TimeOp::Ne.evaluate(11, 10));
        assert!(!TimeOp::Ne.evaluate(10, 10));
    }

    #[test]
    fn test_number_overflow() {
        // Very large number should produce an error, not panic
        let err = parse_pattern("(?99999999999999999999999)").unwrap_err();
        assert!(err.message.contains("overflow"));
    }

    #[test]
    fn test_just_dot() {
        let p = parse_pattern(".").unwrap();
        assert_eq!(p.steps, vec![PatternStep::OneEvent]);
    }

    #[test]
    fn test_just_dot_star() {
        let p = parse_pattern(".*").unwrap();
        assert_eq!(p.steps, vec![PatternStep::AnyEvents]);
    }

    #[test]
    fn test_whitespace_only_rejected() {
        let err = parse_pattern("   ").unwrap_err();
        assert!(err.message.contains("empty"));
    }

    #[test]
    fn test_missing_question_mark() {
        let err = parse_pattern("(1)").unwrap_err();
        assert!(err.message.contains("expected '?'"));
    }

    #[test]
    fn test_invalid_after_question_mark() {
        let err = parse_pattern("(?x)").unwrap_err();
        assert!(err.message.contains("expected digit or 't'"));
    }

    #[test]
    fn test_time_constraint_missing_operator() {
        let err = parse_pattern("(?t100)").unwrap_err();
        assert!(err.message.contains("expected comparison operator"));
    }

    #[test]
    fn test_time_constraint_missing_number() {
        let err = parse_pattern("(?t>=)").unwrap_err();
        assert!(err.message.contains("expected number"));
    }

    #[test]
    fn test_pattern_error_display() {
        let err = PatternError {
            message: "test error".to_string(),
            position: 5,
        };
        assert_eq!(err.to_string(), "pattern error at position 5: test error");
    }

    #[test]
    fn test_pattern_error_is_std_error() {
        let err = PatternError {
            message: "test".to_string(),
            position: 0,
        };
        // Ensure PatternError implements std::error::Error
        let _: &dyn std::error::Error = &err;
    }
}
