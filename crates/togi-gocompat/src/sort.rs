const MAX_INSERTION: usize = 12;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Hint {
    Unknown,
    Increasing,
    Decreasing,
}

fn less(x: f64, y: f64) -> bool {
    (x.is_nan() && !y.is_nan()) || x < y
}

/// Go's `slices.Sort` of a `[]float64`, element for element: NaNs first, and among equal values
/// (`-0` and `+0`, NaNs with different payloads) the order Go's pattern-defeating quicksort leaves.
pub fn sort_f64(data: &mut [f64]) {
    let n = data.len();
    let limit = n.bit_width() as usize;
    pdqsort(data, 0, n, limit);
}

fn insertion_sort(data: &mut [f64], a: usize, b: usize) {
    for i in a + 1..b {
        let mut j = i;
        while j > a && less(data[j], data[j - 1]) {
            data.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn sift_down(data: &mut [f64], lo: usize, hi: usize, first: usize) {
    let mut root = lo;
    loop {
        let mut child = 2 * root + 1;
        if child >= hi {
            return;
        }
        if child + 1 < hi && less(data[first + child], data[first + child + 1]) {
            child += 1;
        }
        if !less(data[first + root], data[first + child]) {
            return;
        }
        data.swap(first + root, first + child);
        root = child;
    }
}

fn heap_sort(data: &mut [f64], a: usize, b: usize) {
    let first = a;
    let hi = b - a;
    for i in (0..=(hi - 1) / 2).rev() {
        sift_down(data, i, hi, first);
    }
    for i in (0..hi).rev() {
        data.swap(first, first + i);
        sift_down(data, 0, i, first);
    }
}

fn pdqsort(data: &mut [f64], mut a: usize, mut b: usize, mut limit: usize) {
    let mut was_balanced = true;
    let mut was_partitioned = true;
    loop {
        let length = b - a;
        if length <= MAX_INSERTION {
            insertion_sort(data, a, b);
            return;
        }
        if limit == 0 {
            heap_sort(data, a, b);
            return;
        }
        if !was_balanced {
            break_patterns(data, a, b);
            limit -= 1;
        }
        let (mut pivot, mut hint) = choose_pivot(data, a, b);
        if hint == Hint::Decreasing {
            data[a..b].reverse();
            pivot = (b - 1) - (pivot - a);
            hint = Hint::Increasing;
        }
        if was_balanced && was_partitioned && hint == Hint::Increasing {
            if partial_insertion_sort(data, a, b) {
                return;
            }
        }
        if a > 0 && !less(data[a - 1], data[pivot]) {
            a = partition_equal(data, a, b, pivot);
            continue;
        }
        let (mid, already_partitioned) = partition(data, a, b, pivot);
        was_partitioned = already_partitioned;
        let (left_len, right_len) = (mid - a, b - mid);
        let balance_threshold = length / 8;
        if left_len < right_len {
            was_balanced = left_len >= balance_threshold;
            pdqsort(data, a, mid, limit);
            a = mid + 1;
        } else {
            was_balanced = right_len >= balance_threshold;
            pdqsort(data, mid + 1, b, limit);
            b = mid;
        }
    }
}

fn partition(data: &mut [f64], a: usize, b: usize, pivot: usize) -> (usize, bool) {
    data.swap(a, pivot);
    let (mut i, mut j) = (a + 1, b - 1);
    while i <= j && less(data[i], data[a]) {
        i += 1;
    }
    while i <= j && !less(data[j], data[a]) {
        j -= 1;
    }
    if i > j {
        data.swap(j, a);
        return (j, true);
    }
    data.swap(i, j);
    i += 1;
    j -= 1;
    loop {
        while i <= j && less(data[i], data[a]) {
            i += 1;
        }
        while i <= j && !less(data[j], data[a]) {
            j -= 1;
        }
        if i > j {
            break;
        }
        data.swap(i, j);
        i += 1;
        j -= 1;
    }
    data.swap(j, a);
    (j, false)
}

fn partition_equal(data: &mut [f64], a: usize, b: usize, pivot: usize) -> usize {
    data.swap(a, pivot);
    let (mut i, mut j) = (a + 1, b - 1);
    loop {
        while i <= j && !less(data[a], data[i]) {
            i += 1;
        }
        while i <= j && less(data[a], data[j]) {
            j -= 1;
        }
        if i > j {
            break;
        }
        data.swap(i, j);
        i += 1;
        j -= 1;
    }
    i
}

fn partial_insertion_sort(data: &mut [f64], a: usize, b: usize) -> bool {
    const MAX_STEPS: usize = 5;
    const SHORTEST_SHIFTING: usize = 50;
    let mut i = a + 1;
    for _ in 0..MAX_STEPS {
        while i < b && !less(data[i], data[i - 1]) {
            i += 1;
        }
        if i == b {
            return true;
        }
        if b - a < SHORTEST_SHIFTING {
            return false;
        }
        data.swap(i, i - 1);
        if i - a >= 2 {
            let mut j = i - 1;
            while j >= 1 {
                if !less(data[j], data[j - 1]) {
                    break;
                }
                data.swap(j, j - 1);
                j -= 1;
            }
        }
        if b - i >= 2 {
            let mut j = i + 1;
            while j < b {
                if !less(data[j], data[j - 1]) {
                    break;
                }
                data.swap(j, j - 1);
                j += 1;
            }
        }
    }
    false
}

fn break_patterns(data: &mut [f64], a: usize, b: usize) {
    let length = b - a;
    if length < 8 {
        return;
    }
    let mut random = length as u64;
    let modulus = 1_u64 << length.bit_width();
    let middle = a + (length / 4) * 2;
    for idx in middle - 1..=middle + 1 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the masked value is below twice the length, which is a usize"
        )]
        let mut other = (random & (modulus - 1)) as usize;
        if other >= length {
            other -= length;
        }
        data.swap(idx, a + other);
    }
}

fn choose_pivot(data: &[f64], a: usize, b: usize) -> (usize, Hint) {
    const SHORTEST_NINTHER: usize = 50;
    const MAX_SWAPS: usize = 4 * 3;
    let length = b - a;
    let mut swaps = 0;
    let mut quarter = a + length / 4;
    let mut middle = a + length / 4 * 2;
    let mut three_quarters = a + length / 4 * 3;
    if length >= 8 {
        if length >= SHORTEST_NINTHER {
            quarter = median_adjacent(data, quarter, &mut swaps);
            middle = median_adjacent(data, middle, &mut swaps);
            three_quarters = median_adjacent(data, three_quarters, &mut swaps);
        }
        middle = median(data, quarter, middle, three_quarters, &mut swaps);
    }
    match swaps {
        0 => (middle, Hint::Increasing),
        MAX_SWAPS => (middle, Hint::Decreasing),
        _ => (middle, Hint::Unknown),
    }
}

fn order2(data: &[f64], a: usize, b: usize, swaps: &mut usize) -> (usize, usize) {
    if less(data[b], data[a]) {
        *swaps += 1;
        (b, a)
    } else {
        (a, b)
    }
}

fn median(data: &[f64], a: usize, b: usize, c: usize, swaps: &mut usize) -> usize {
    let (a, b) = order2(data, a, b, swaps);
    let (b, _) = order2(data, b, c, swaps);
    order2(data, a, b, swaps).1
}

fn median_adjacent(data: &[f64], a: usize, swaps: &mut usize) -> usize {
    median(data, a - 1, a, a + 1, swaps)
}

#[cfg(test)]
mod tests {
    use super::sort_f64;
    use std::fs;
    use std::path::Path;

    fn bits(text: &str) -> Vec<f64> {
        text.split(',')
            .filter(|field| !field.is_empty())
            .map(|field| f64::from_bits(u64::from_str_radix(field, 16).unwrap()))
            .collect()
    }

    #[test]
    fn sort_matches_go_bit_for_bit() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("sort.txt");
        let text = fs::read_to_string(path).unwrap();
        assert!(text.ends_with('\n'));
        let mut rows = 0;
        for line in text.lines() {
            let mut fields = line.split('\t');
            assert_eq!(fields.next(), Some("sort"));
            let mut data = bits(fields.next().unwrap());
            let want = fields.next().unwrap();
            sort_f64(&mut data);
            let got: Vec<u64> = data.iter().map(|value| value.to_bits()).collect();
            let want: Vec<u64> = bits(want).iter().map(|value| value.to_bits()).collect();
            assert_eq!(got, want, "row {rows}");
            rows += 1;
        }
        assert_eq!(rows, 98);
    }
}
