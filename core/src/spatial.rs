//! Morton-ordered quadtree. Items are sorted by the Z-order code of their
//! centre, so every quadtree node is one contiguous index range: culling
//! returns a handful of ranges that map straight onto instanced draws.

const LEAF: usize = 256;
const MAX_DEPTH: u32 = 16;

#[derive(Clone, Copy)]
struct Node {
    bbox: [f32; 4],
    start: u32,
    end: u32,
    /// Largest item size inside, for level-of-detail skipping.
    max_size: f32,
    first_child: u32,
    child_count: u32,
}

pub struct Index {
    nodes: Vec<Node>,
}

fn spread(v: u32) -> u32 {
    let mut x = v & 0xffff;
    x = (x | (x << 8)) & 0x00ff00ff;
    x = (x | (x << 4)) & 0x0f0f0f0f;
    x = (x | (x << 2)) & 0x33333333;
    x = (x | (x << 1)) & 0x55555555;
    x
}

fn intersects(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

fn contains(outer: &[f32; 4], inner: &[f32; 4]) -> bool {
    inner[0] >= outer[0] && inner[2] <= outer[2] && inner[1] >= outer[1] && inner[3] <= outer[3]
}

impl Index {
    /// Returns the permutation (new order -> old index) and the index.
    /// The caller must reorder its item arrays by the permutation.
    pub fn build(bboxes: &[[f32; 4]], sizes: &[f32]) -> (Vec<u32>, Index) {
        let n = bboxes.len();
        if n == 0 {
            return (Vec::new(), Index { nodes: Vec::new() });
        }
        let mut world = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for b in bboxes {
            world[0] = world[0].min(b[0]);
            world[1] = world[1].min(b[1]);
            world[2] = world[2].max(b[2]);
            world[3] = world[3].max(b[3]);
        }
        let span = (world[2] - world[0]).max(world[3] - world[1]).max(1e-3);
        let codes: Vec<u32> = bboxes
            .iter()
            .map(|b| {
                let cx = ((b[0] + b[2]) * 0.5 - world[0]) / span;
                let cy = ((b[1] + b[3]) * 0.5 - world[1]) / span;
                let qx = (cx.clamp(0.0, 1.0) * 65535.0) as u32;
                let qy = (cy.clamp(0.0, 1.0) * 65535.0) as u32;
                spread(qx) | spread(qy) << 1
            })
            .collect();
        let mut perm: Vec<u32> = (0..n as u32).collect();
        perm.sort_unstable_by_key(|&i| codes[i as usize]);
        let sorted_codes: Vec<u32> = perm.iter().map(|&i| codes[i as usize]).collect();
        let sorted_bb: Vec<[f32; 4]> = perm.iter().map(|&i| bboxes[i as usize]).collect();
        let sorted_sz: Vec<f32> = perm.iter().map(|&i| sizes[i as usize]).collect();

        let mut idx = Index { nodes: Vec::with_capacity(n / LEAF * 2 + 8) };
        idx.nodes.push(Node { bbox: [0.0; 4], start: 0, end: n as u32, max_size: 0.0, first_child: 0, child_count: 0 });
        idx.build_node(0, 0, &sorted_codes, &sorted_bb, &sorted_sz);
        (perm, idx)
    }

    fn build_node(&mut self, ni: usize, depth: u32, codes: &[u32], bb: &[[f32; 4]], sz: &[f32]) {
        let (s, e) = (self.nodes[ni].start as usize, self.nodes[ni].end as usize);
        if e - s <= LEAF || depth >= MAX_DEPTH {
            let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            let mut m = 0f32;
            for i in s..e {
                b[0] = b[0].min(bb[i][0]);
                b[1] = b[1].min(bb[i][1]);
                b[2] = b[2].max(bb[i][2]);
                b[3] = b[3].max(bb[i][3]);
                m = m.max(sz[i]);
            }
            self.nodes[ni].bbox = b;
            self.nodes[ni].max_size = m;
            return;
        }
        // Split by the next two Morton bits.
        let shift = 30 - depth * 2;
        let mut bounds = [s; 5];
        bounds[4] = e;
        for q in 1..4u32 {
            let start = bounds[q as usize - 1];
            let off = codes[start..e].partition_point(|&c| (c >> shift) & 3 < q);
            bounds[q as usize] = start + off;
        }
        let first = self.nodes.len() as u32;
        let mut count = 0;
        for q in 0..4 {
            if bounds[q + 1] > bounds[q] {
                self.nodes.push(Node {
                    bbox: [0.0; 4],
                    start: bounds[q] as u32,
                    end: bounds[q + 1] as u32,
                    max_size: 0.0,
                    first_child: 0,
                    child_count: 0,
                });
                count += 1;
            }
        }
        self.nodes[ni].first_child = first;
        self.nodes[ni].child_count = count;
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        let mut m = 0f32;
        for c in first..first + count {
            self.build_node(c as usize, depth + 1, codes, bb, sz);
            let cn = self.nodes[c as usize];
            b[0] = b[0].min(cn.bbox[0]);
            b[1] = b[1].min(cn.bbox[1]);
            b[2] = b[2].max(cn.bbox[2]);
            b[3] = b[3].max(cn.bbox[3]);
            m = m.max(cn.max_size);
        }
        self.nodes[ni].bbox = b;
        self.nodes[ni].max_size = m;
    }

    /// Appends sorted, merged index ranges of items that may intersect `view`
    /// and whose size is at least `min_size`.
    pub fn query(&self, view: &[f32; 4], min_size: f32, out: &mut Vec<(u32, u32)>) {
        out.clear();
        if self.nodes.is_empty() {
            return;
        }
        let mut stack = vec![0u32];
        while let Some(ni) = stack.pop() {
            let n = &self.nodes[ni as usize];
            if !intersects(&n.bbox, view) || n.max_size < min_size {
                continue;
            }
            if n.child_count == 0 || contains(view, &n.bbox) {
                match out.last_mut() {
                    Some(last) if last.1 == n.start => last.1 = n.end,
                    _ => out.push((n.start, n.end)),
                }
                continue;
            }
            for c in (n.first_child..n.first_child + n.child_count).rev() {
                stack.push(c);
            }
        }
    }
}

pub fn reorder<T: Clone>(v: &mut Vec<T>, perm: &[u32]) {
    let sorted: Vec<T> = perm.iter().map(|&i| v[i as usize].clone()).collect();
    *v = sorted;
}
