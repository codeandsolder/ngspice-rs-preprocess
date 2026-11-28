//! Wire endpoint and pin connectivity. Interior-only geometric crossings
//! have no electrical node unless an explicit endpoint, pin or flag is present.

use crate::parse::{Point, Sheet};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::collections::{BTreeMap, BTreeSet};

struct UnionFind(Vec<usize>);

impl UnionFind {
    fn root(&mut self, mut id: usize) -> usize {
        while self.0[id] != id {
            self.0[id] = self.0[self.0[id]];
            id = self.0[id];
        }
        id
    }
    fn join(&mut self, a: usize, b: usize) {
        let a = self.root(a);
        let b = self.root(b);
        self.0[a.max(b)] = a.min(b);
    }
}

fn on_segment(p: Point, a: Point, b: Point) -> bool {
    let (dx, dy) = (i128::from(b.0 - a.0), i128::from(b.1 - a.1));
    let (px, py) = (i128::from(p.0 - a.0), i128::from(p.1 - a.1));
    dx * py == dy * px
        && (a.0.min(b.0)..=a.0.max(b.0)).contains(&p.0)
        && (a.1.min(b.1)..=a.1.max(b.1)).contains(&p.1)
}

fn electrical_points(
    sheet: &Sheet,
    pins: impl IntoIterator<Item = Point>,
    budget: &WorkBudget,
) -> Result<(BTreeSet<Point>, BTreeSet<Point>)> {
    let mut unique = BTreeSet::new();
    let mut attached = BTreeSet::new();
    for point in pins {
        // Charge each occurrence before allocating topology storage;
        // repeated placements of a large cached symbol must remain bounded.
        budget.charge(Resource::GeneratedNodes, 1)?;
        if !unique.insert(point) {
            attached.insert(point);
        }
    }
    for point in sheet
        .labels
        .iter()
        .map(|label| label.point)
        .chain(sheet.ports.keys().copied())
    {
        budget.charge(Resource::GeneratedNodes, 1)?;
        unique.insert(point);
        attached.insert(point);
    }
    for point in sheet
        .wires
        .iter()
        .flat_map(|&wire| <[Point; 2]>::from(wire))
    {
        budget.charge(Resource::GeneratedNodes, 1)?;
        unique.insert(point);
        attached.insert(point);
    }
    Ok((unique, attached))
}

struct Node {
    name: String,
    attached: bool,
}

pub struct Nets(BTreeMap<Point, Node>);

impl Nets {
    pub fn name(&self, point: Point) -> Result<&str> {
        self.0
            .get(&point)
            .map(|node| node.name.as_str())
            .context("missing topology point")
    }

    /// A physical terminal is attached when a wire, flag or another pin
    /// touches its point. A dangling wire still counts as a connection.
    pub fn attached_name(&self, point: Point) -> Result<Option<&str>> {
        let node = self.0.get(&point).context("missing topology point")?;
        Ok(node.attached.then_some(node.name.as_str()))
    }

    pub fn build(
        sheet: &Sheet,
        pins: impl IntoIterator<Item = Point>,
        budget: &WorkBudget,
    ) -> Result<Self> {
        let (unique, attached) = electrical_points(sheet, pins, budget)?;
        let indices: BTreeMap<_, _> = unique
            .into_iter()
            .enumerate()
            .map(|(index, point)| (point, index))
            .collect();
        let by_y: BTreeMap<_, _> = indices
            .iter()
            .map(|(&Point(x, y), &index)| (Point(y, x), index))
            .collect();
        let mut sets = UnionFind((0..indices.len()).collect());
        let mut connected = vec![false; indices.len()];
        for point in attached {
            connected[indices[&point]] = true;
        }
        for &(a, b) in &sheet.wires {
            let (index, first, second) = if (b.0 - a.0).abs() >= (b.1 - a.1).abs() {
                (&indices, a, b)
            } else {
                (&by_y, Point(a.1, a.0), Point(b.1, b.0))
            };
            let anchor = indices[&a];
            for (&point, &id) in index.range(
                Point(first.0.min(second.0), i64::MIN)..=Point(first.0.max(second.0), i64::MAX),
            ) {
                budget.charge(Resource::EvaluationSteps, 1)?;
                if on_segment(point, first, second) {
                    sets.join(anchor, id);
                    connected[id] = true;
                }
            }
        }
        let mut labels = BTreeMap::new();
        for label in &sheet.labels {
            let id = indices[&label.point];
            let name = label.name.to_ascii_lowercase();
            if let Some(previous) = labels.insert(name, id) {
                sets.join(previous, id);
            }
        }
        let mut names = BTreeMap::new();
        let mut used = BTreeSet::new();
        for label in &sheet.labels {
            let root = sets.root(indices[&label.point]);
            let name = label.name.to_ascii_lowercase();
            ensure!(
                !name.is_empty()
                    && !name.starts_with('$')
                    && !name.chars().any(|c| c.is_whitespace()
                        || matches!(c, '(' | ')' | ',' | ';' | '"' | '\'' | '=' | '{' | '}')),
                "unsupported or invalid net label {} at line {}",
                label.name,
                label.line
            );
            if let Some(previous) = names.insert(root, name.clone()) {
                ensure!(
                    previous == name,
                    "conflicting labels {previous} and {name} on one electrical node (line {})",
                    label.line
                );
            }
            used.insert(name);
        }
        let mut next = 1;
        let mut result = BTreeMap::new();
        for (point, id) in indices {
            let root = sets.root(id);
            let name = names.entry(root).or_insert_with(|| {
                loop {
                    let name = format!("n{next:03}");
                    next += 1;
                    if used.insert(name.clone()) {
                        break name;
                    }
                }
            });
            result.insert(
                point,
                Node {
                    name: name.clone(),
                    attached: connected[id],
                },
            );
        }
        Ok(Self(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossings_and_junctions_are_distinct() -> Result<()> {
        let budget = WorkBudget::default();
        let sheet = Sheet::parse(
            "Version 4\nSHEET 1 800 600\nWIRE 0 10 20 10\nWIRE 10 0 10 20\n",
            &budget,
        )?;
        let nets = Nets::build(&sheet, [], &budget)?;
        assert_eq!(nets.name(Point(0, 10))?, nets.name(Point(20, 10))?);
        assert_ne!(nets.name(Point(0, 10))?, nets.name(Point(10, 0))?);
        let nets = Nets::build(&sheet, [Point(10, 10)], &budget)?;
        assert_eq!(nets.name(Point(0, 10))?, nets.name(Point(10, 0))?);
        Ok(())
    }

    #[test]
    fn branches_overlaps_diagonals_and_named_islands() -> Result<()> {
        let budget = WorkBudget::default();
        let sheet = Sheet::parse(
            "Version 4\nSHEET 1 800 600\nWIRE 0 0 20 20\nWIRE 10 10 30 10\nWIRE 15 15 25 25\nFLAG 30 10 OUT\nFLAG 100 100 out\nFLAG 0 100 n001\nFLAG 0 200 0\n",
            &budget,
        )?;
        let nets = Nets::build(&sheet, [Point(8, 8), Point(5, 6), Point(6, 8)], &budget)?;
        for point in [Point(0, 0), Point(25, 25), Point(100, 100), Point(8, 8)] {
            assert_eq!(nets.name(point)?, "out");
        }
        assert_ne!(nets.name(Point(5, 6))?, "n001");
        assert_ne!(nets.name(Point(5, 6))?, nets.name(Point(6, 8))?);
        assert_eq!(nets.name(Point(0, 200))?, "0");
        Ok(())
    }

    #[test]
    fn conflicting_labels_are_diagnosed() -> Result<()> {
        let budget = WorkBudget::default();
        let sheet = Sheet::parse(
            "Version 4\nSHEET 1 100 100\nWIRE 0 0 10 10\nFLAG 0 0 left\nFLAG 10 10 right\n",
            &budget,
        )?;
        assert!(Nets::build(&sheet, [], &budget).is_err());
        Ok(())
    }
}
