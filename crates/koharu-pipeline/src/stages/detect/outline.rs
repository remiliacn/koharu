//! A block's mask grown over its lettering's outline: the segmentation
//! covers the glyphs and the inner part of a thick outline, and the mask's
//! fixed growth doesn't reach a stroke wider than it, so the outer stroke
//! would survive inpainting.

use std::collections::VecDeque;

use image::{GrayImage, ImageBuffer, Luma, Rgb, RgbImage, imageops};
use imageproc::{
    distance_transform::{Norm, distance_transform},
    morphology::dilate,
    region_labelling::{Connectivity, connected_components},
};
use koharu_scene::Rect;
use rayon::prelude::*;

const COLOR_TOLERANCE: u8 = 32;
const FRINGE: u8 = 2;
const MIN_BORDER_PIXELS: usize = 32;
const MIN_BORDER_SHARE: f64 = 0.25;
/// A grown region is rejected once its pixels at the reach limit reach a
/// quarter of its pixels next to the mask.
const MAX_LIMIT_CONTACT: usize = 4;

/// The mask `pixels` over `rect` on `page`, grown over the outline around
/// its lettering but into none of `others`, and the rectangle it then
/// covers; the mask as it was when no outline is found.
pub(super) fn outlined(
    page: &RgbImage,
    rect: Rect,
    pixels: Vec<u8>,
    others: &[[f32; 4]],
) -> (Rect, Vec<u8>) {
    let (width, height) = page.dimensions();
    let radius = ((width.max(height) as f32 / 1024.0) * 6.0)
        .round()
        .clamp(1.0, 255.0) as u8;
    let margin = i64::from(radius) * 2 + 1 + i64::from(FRINGE);
    let (x0, y0) = (
        (i64::from(rect.x) - margin).max(0),
        (i64::from(rect.y) - margin).max(0),
    );
    let x1 = (i64::from(rect.x) + i64::from(rect.width) + margin).min(i64::from(width));
    let y1 = (i64::from(rect.y) + i64::from(rect.height) + margin).min(i64::from(height));
    let outer = Rect {
        x: x0 as i32,
        y: y0 as i32,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
    };
    let (dx, dy) = ((rect.x - outer.x) as u32, (rect.y - outer.y) as u32);
    let mut mask = GrayImage::new(outer.width, outer.height);
    for (at, &value) in pixels.iter().enumerate() {
        let (x, y) = (at as u32 % rect.width, at as u32 / rect.width);
        mask.put_pixel(x + dx, y + dy, Luma([value]));
    }
    let image = imageops::crop_imm(
        page,
        outer.x as u32,
        outer.y as u32,
        outer.width,
        outer.height,
    )
    .to_image();
    let blocked = |x: u32, y: u32| {
        let (x, y) = ((outer.x as u32 + x) as f32 + 0.5, (outer.y as u32 + y) as f32 + 0.5);
        others.iter().any(|&[left, top, right, bottom]| {
            (left..=right).contains(&x) && (top..=bottom).contains(&y)
        })
    };
    if !grow_into_outlines(&image, &mut mask, radius, &blocked) {
        return (rect, pixels);
    }
    (outer, mask.into_raw())
}

/// Grows `mask` over each of its components' outline: when one colour
/// dominates a component's border band, over the connected pixels of that
/// colour and their anti-aliased edge, up to twice `radius` out, but onto no
/// `blocked` pixel. Whether it grew.
fn grow_into_outlines(
    image: &RgbImage,
    mask: &mut GrayImage,
    radius: u8,
    blocked: &(dyn Fn(u32, u32) -> bool + Sync),
) -> bool {
    let binary = GrayImage::from_fn(mask.width(), mask.height(), |x, y| {
        Luma([if mask.get_pixel(x, y)[0] >= 127 {
            u8::MAX
        } else {
            0
        }])
    });
    let labels = connected_components(&binary, Connectivity::Eight, Luma([0]));
    let mut bounds = Vec::<Option<[u32; 4]>>::new();
    for (x, y, pixel) in labels.enumerate_pixels() {
        let label = pixel[0] as usize;
        if label == 0 {
            continue;
        }
        if label >= bounds.len() {
            bounds.resize(label + 1, None);
        }
        let bounds = bounds[label].get_or_insert([x, y, x + 1, y + 1]);
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].min(y);
        bounds[2] = bounds[2].max(x + 1);
        bounds[3] = bounds[3].max(y + 1);
    }
    let grown = bounds
        .into_iter()
        .enumerate()
        .filter_map(|(label, bounds)| Some((label as u32, bounds?)))
        .collect::<Vec<_>>()
        .into_par_iter()
        .filter_map(|(label, bounds)| growth(image, &labels, label, bounds, radius, blocked))
        .collect::<Vec<_>>();
    let mut changed = false;
    for (left, top, growth) in grown {
        for (x, y, pixel) in growth.enumerate_pixels() {
            let (x, y) = (left + x, top + y);
            if pixel[0] != 0 && !blocked(x, y) && mask.get_pixel(x, y)[0] != u8::MAX {
                mask.put_pixel(x, y, Luma([u8::MAX]));
                changed = true;
            }
        }
    }
    changed
}

fn growth(
    image: &RgbImage,
    labels: &ImageBuffer<Luma<u32>, Vec<u32>>,
    label: u32,
    [component_left, component_top, component_right, component_bottom]: [u32; 4],
    radius: u8,
    blocked: &(dyn Fn(u32, u32) -> bool + Sync),
) -> Option<(u32, u32, GrayImage)> {
    let reach = u32::from(radius) * 2;
    let margin = reach + 1;
    let left = component_left.saturating_sub(margin);
    let top = component_top.saturating_sub(margin);
    let width = component_right.saturating_add(margin).min(image.width()) - left;
    let height = component_bottom.saturating_add(margin).min(image.height()) - top;
    let component = GrayImage::from_fn(width, height, |x, y| {
        Luma([if labels.get_pixel(left + x, top + y)[0] == label {
            u8::MAX
        } else {
            0
        }])
    });
    let outside = distance_transform(&component, Norm::LInf);
    let mut inverted = component.clone();
    inverted
        .iter_mut()
        .for_each(|value| *value = u8::MAX - *value);
    let inside = distance_transform(&inverted, Norm::LInf);
    let pixel = |x: u32, y: u32| *image.get_pixel(left + x, top + y);

    let border = inside
        .enumerate_pixels()
        .filter(|(_, _, depth)| depth[0] != 0 && depth[0] <= radius)
        .map(|(x, y, _)| pixel(x, y))
        .collect::<Vec<_>>();
    if border.len() < MIN_BORDER_PIXELS {
        return None;
    }
    let bin = |color: &Rgb<u8>| {
        (usize::from(color[0] >> 4) << 8)
            | (usize::from(color[1] >> 4) << 4)
            | usize::from(color[2] >> 4)
    };
    let mut counts = vec![0_usize; 1 << 12];
    for color in &border {
        counts[bin(color)] += 1;
    }
    let dominant = (0..counts.len()).max_by_key(|index| counts[*index])?;
    let members = border
        .iter()
        .filter(|color| bin(color) == dominant)
        .collect::<Vec<_>>();
    let stroke = Rgb(std::array::from_fn(|channel| {
        (members
            .iter()
            .map(|color| usize::from(color[channel]))
            .sum::<usize>()
            / members.len()) as u8
    }));
    let matches = |color: &Rgb<u8>| {
        (0..3).all(|channel| color[channel].abs_diff(stroke[channel]) <= COLOR_TOLERANCE)
    };
    let border_share =
        border.iter().filter(|color| matches(color)).count() as f64 / border.len() as f64;
    if border_share < MIN_BORDER_SHARE {
        return None;
    }

    let open = |x: u32, y: u32| matches(&pixel(x, y)) && !blocked(left + x, top + y);
    let mut grown = GrayImage::new(width, height);
    let mut queue = VecDeque::new();
    for (x, y, distance) in outside.enumerate_pixels() {
        if distance[0] == 1 && open(x, y) {
            grown.put_pixel(x, y, Luma([u8::MAX]));
            queue.push_back((x, y));
        }
    }
    if queue.is_empty() {
        return None;
    }
    while let Some((x, y)) = queue.pop_front() {
        for (dx, dy) in [
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ] {
            let (Some(nx), Some(ny)) = (x.checked_add_signed(dx), y.checked_add_signed(dy)) else {
                continue;
            };
            if nx >= width || ny >= height || grown.get_pixel(nx, ny)[0] != 0 {
                continue;
            }
            let distance = u32::from(outside.get_pixel(nx, ny)[0]);
            if distance == 0 || distance > reach || !open(nx, ny) {
                continue;
            }
            grown.put_pixel(nx, ny, Luma([u8::MAX]));
            queue.push_back((nx, ny));
        }
    }

    // A stroke ends before the reach limit. A flood that keeps running into the
    // limit is a background of the stroke's color, not part of the lettering.
    let regions = connected_components(&grown, Connectivity::Eight, Luma([0]));
    let mut contacts = Vec::<(usize, usize)>::new();
    for (x, y, region) in regions.enumerate_pixels() {
        let region = region[0] as usize;
        if region == 0 {
            continue;
        }
        if region >= contacts.len() {
            contacts.resize(region + 1, (0, 0));
        }
        match u32::from(outside.get_pixel(x, y)[0]) {
            1 => contacts[region].0 += 1,
            distance if distance == reach => contacts[region].1 += 1,
            _ => {}
        }
    }
    let mut accepted = false;
    for (x, y, region) in regions.enumerate_pixels() {
        let (seeds, limit) = contacts
            .get(region[0] as usize)
            .copied()
            .unwrap_or_default();
        let keep = region[0] != 0 && limit * MAX_LIMIT_CONTACT < seeds;
        accepted |= keep;
        grown.put_pixel(x, y, Luma([if keep { u8::MAX } else { 0 }]));
    }
    accepted.then(|| (left, top, dilate(&grown, Norm::LInf, FRINGE)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_mask(size: u32, [left, top, right, bottom]: [u32; 4]) -> GrayImage {
        GrayImage::from_fn(size, size, |x, y| {
            Luma([
                if (left..right).contains(&x) && (top..bottom).contains(&y) {
                    u8::MAX
                } else {
                    0
                },
            ])
        })
    }

    #[test]
    fn a_mask_grows_over_an_outline_wider_than_its_growth() {
        let image = RgbImage::from_fn(80, 80, |x, y| {
            if (28..52).contains(&x) && (28..52).contains(&y) {
                Rgb([255, 40, 170])
            } else if (20..60).contains(&x) && (20..60).contains(&y) {
                Rgb([255, 255, 255])
            } else {
                Rgb([40, 40, 80])
            }
        });
        let mut mask = square_mask(80, [25, 25, 55, 55]);

        assert!(grow_into_outlines(&image, &mut mask, 3, &|_, _| false));

        assert_eq!(mask.get_pixel(20, 40)[0], u8::MAX);
        assert_eq!(mask.get_pixel(59, 59)[0], u8::MAX);
        assert_eq!(mask.get_pixel(61, 40)[0], u8::MAX, "anti-aliased edge");
        assert_eq!(mask.get_pixel(70, 40)[0], 0);
    }

    #[test]
    fn a_mask_does_not_grow_into_a_background_of_the_border_color() {
        let image = RgbImage::from_fn(80, 80, |x, y| {
            if (30..50).contains(&x) && (30..50).contains(&y) {
                Rgb([0, 0, 0])
            } else {
                Rgb([255, 255, 255])
            }
        });
        let mut mask = square_mask(80, [27, 27, 53, 53]);
        let expected = mask.clone();

        assert!(!grow_into_outlines(&image, &mut mask, 3, &|_, _| false));

        assert_eq!(mask, expected);
    }

    #[test]
    fn an_outline_stops_at_another_lettering() {
        let image = RgbImage::from_fn(80, 80, |x, y| {
            if (28..52).contains(&x) && (28..52).contains(&y) {
                Rgb([255, 40, 170])
            } else if (20..60).contains(&x) && (20..60).contains(&y) {
                Rgb([255, 255, 255])
            } else {
                Rgb([40, 40, 80])
            }
        });
        let mut mask = square_mask(80, [25, 25, 55, 55]);

        grow_into_outlines(&image, &mut mask, 3, &|x, _| x < 24);

        assert_eq!(mask.get_pixel(20, 40)[0], 0);
        assert_eq!(mask.get_pixel(59, 59)[0], u8::MAX);
    }
}
