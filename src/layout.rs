use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Rect {
    fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Horizontal,
    Vertical,
}

impl Direction {
    pub fn action(self) -> &'static str {
        match self {
            Self::Horizontal => "right",
            Self::Vertical => "down",
        }
    }

    fn start(self, rect: Rect) -> i64 {
        match self {
            Self::Horizontal => rect.x,
            Self::Vertical => rect.y,
        }
    }

    fn size(self, rect: Rect) -> i64 {
        match self {
            Self::Horizontal => rect.width,
            Self::Vertical => rect.height,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Layout {
    Leaf {
        terminal: usize,
    },
    Split {
        direction: Direction,
        first_size: i64,
        total_size: i64,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    pub fn infer(frames: &[(usize, Rect)]) -> Option<Self> {
        if frames
            .iter()
            .any(|(_, rect)| rect.width <= 0 || rect.height <= 0)
        {
            return None;
        }
        if let [(terminal, _)] = frames {
            return Some(Self::Leaf {
                terminal: *terminal,
            });
        }
        let bounds = frames.iter().map(|(_, rect)| *rect).reduce(Rect::union)?;
        for direction in [Direction::Horizontal, Direction::Vertical] {
            let mut cuts = frames
                .iter()
                .map(|(_, rect)| direction.start(*rect) + direction.size(*rect))
                .collect::<Vec<_>>();
            cuts.sort_unstable();
            cuts.dedup();
            for cut in cuts {
                let (first, second): (Vec<_>, Vec<_>) = frames
                    .iter()
                    .copied()
                    .partition(|(_, rect)| direction.start(*rect) + direction.size(*rect) <= cut);
                if first.is_empty()
                    || second.is_empty()
                    || second.iter().any(|(_, rect)| direction.start(*rect) < cut)
                {
                    continue;
                }
                let first_bounds = first.iter().map(|(_, rect)| *rect).reduce(Rect::union)?;
                let second_bounds = second.iter().map(|(_, rect)| *rect).reduce(Rect::union)?;
                let gap = direction.start(second_bounds)
                    - direction.start(first_bounds)
                    - direction.size(first_bounds);
                let perpendicular = match direction {
                    Direction::Horizontal => Direction::Vertical,
                    Direction::Vertical => Direction::Horizontal,
                };
                if !(0..=2).contains(&gap)
                    || perpendicular.start(first_bounds) != perpendicular.start(bounds)
                    || perpendicular.start(second_bounds) != perpendicular.start(bounds)
                    || perpendicular.size(first_bounds) != perpendicular.size(bounds)
                    || perpendicular.size(second_bounds) != perpendicular.size(bounds)
                {
                    continue;
                }
                if let (Some(first), Some(second)) = (Self::infer(&first), Self::infer(&second)) {
                    return Some(Self::Split {
                        direction,
                        first_size: direction.size(first_bounds),
                        total_size: direction.size(bounds),
                        first: Box::new(first),
                        second: Box::new(second),
                    });
                }
            }
        }
        None
    }

    pub fn first_terminal(&self) -> usize {
        match self {
            Self::Leaf { terminal } => *terminal,
            Self::Split { first, .. } => first.first_terminal(),
        }
    }

    pub fn valid_for(&self, terminals: usize) -> bool {
        let mut leaves = Vec::new();
        if !self.collect_valid_leaves(&mut leaves) {
            return false;
        }
        leaves.sort_unstable();
        leaves == (1..=terminals).collect::<Vec<_>>()
    }

    fn collect_valid_leaves(&self, leaves: &mut Vec<usize>) -> bool {
        match self {
            Self::Leaf { terminal } => {
                leaves.push(*terminal);
                true
            }
            Self::Split {
                first_size,
                total_size,
                first,
                second,
                ..
            } => {
                *first_size > 0
                    && *first_size < *total_size
                    && first.collect_valid_leaves(leaves)
                    && second.collect_valid_leaves(leaves)
            }
        }
    }

    pub fn description(&self) -> &'static str {
        let (horizontal, vertical) = self.directions();
        match (horizontal, vertical) {
            (false, false) => "single pane",
            (true, false) => "left/right",
            (false, true) => "top/bottom",
            (true, true) => "left/right + top/bottom",
        }
    }

    fn directions(&self) -> (bool, bool) {
        match self {
            Self::Leaf { .. } => (false, false),
            Self::Split {
                direction,
                first,
                second,
                ..
            } => {
                let a = first.directions();
                let b = second.directions();
                (
                    *direction == Direction::Horizontal || a.0 || b.0,
                    *direction == Direction::Vertical || a.1 || b.1,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(terminal: usize, x: i64, y: i64, width: i64, height: i64) -> (usize, Rect) {
        (
            terminal,
            Rect {
                x,
                y,
                width,
                height,
            },
        )
    }

    #[test]
    fn reconstructs_mixed_splits_and_exact_divider_sizes() {
        let frames = [
            frame(1, 78, 127, 696, 452),
            frame(2, 78, 579, 696, 287),
            frame(3, 774, 127, 403, 308),
            frame(4, 774, 435, 403, 431),
        ];
        let layout = Layout::infer(&frames).unwrap();
        assert!(layout.valid_for(4));
        assert_eq!(layout.description(), "left/right + top/bottom");
        let Layout::Split {
            direction,
            first_size,
            total_size,
            first,
            second,
        } = layout
        else {
            panic!("expected a split")
        };
        assert_eq!(direction, Direction::Horizontal);
        assert_eq!((first_size, total_size), (696, 1099));
        assert!(matches!(
            *first,
            Layout::Split {
                direction: Direction::Vertical,
                first_size: 452,
                total_size: 739,
                ..
            }
        ));
        assert!(matches!(
            *second,
            Layout::Split {
                direction: Direction::Vertical,
                first_size: 308,
                total_size: 739,
                ..
            }
        ));
    }

    #[test]
    fn refuses_missing_overlapping_and_invalid_panes() {
        assert!(Layout::infer(&[]).is_none());
        assert!(Layout::infer(&[frame(1, 0, 0, 50, 50), frame(2, 25, 25, 50, 50)]).is_none());
        assert!(Layout::infer(&[frame(1, 0, 0, 0, 50)]).is_none());
        let layout = Layout::infer(&[frame(1, 0, 0, 50, 50), frame(1, 50, 0, 50, 50)]).unwrap();
        assert!(!layout.valid_for(2));
    }
}
