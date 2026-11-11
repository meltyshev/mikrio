//! Whole-string IAM wildcard matching, never filesystem or URL normalization.

/// Deterministic O(pattern characters * value characters), O(value characters)
/// memory. No regex compiler, recursion or exponential wildcard backtracking.
/// Callers bound complete policy bytes and operation-context values first.
pub(super) fn matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    if !pattern.contains(['*', '?']) {
        return pattern == value;
    }

    let characters: Vec<_> = value.chars().collect();

    if pattern.chars().filter(|token| *token != '*').count() > characters.len() {
        return false;
    }

    let mut previous = vec![false; characters.len() + 1];
    let mut current = vec![false; characters.len() + 1];
    previous[0] = true;
    let mut previous_star = false;

    for token in pattern.chars() {
        if token == '*' && previous_star {
            continue;
        }

        previous_star = token == '*';
        current[0] = token == '*' && previous[0];

        for (index, character) in characters.iter().enumerate() {
            current[index + 1] = match token {
                '*' => current[index] || previous[index + 1],
                '?' => previous[index],
                literal => previous[index] && literal == *character,
            };
        }

        std::mem::swap(&mut previous, &mut current);
    }

    previous[characters.len()]
}
