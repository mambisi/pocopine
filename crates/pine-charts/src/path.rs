use core::fmt::Write;

use crate::error::{finite, ChartError, ChartResult};
use crate::geometry::Point;

pub fn line_path(points: impl IntoIterator<Item = Point>) -> ChartResult<String> {
    let mut points = points.into_iter();
    let Some(first) = points.next() else {
        return Err(ChartError::EmptySeries);
    };

    let mut output = String::new();
    write_point(&mut output, "M", first)?;

    for point in points {
        write_point(&mut output, "L", point)?;
    }

    Ok(output)
}

pub fn area_path(points: impl IntoIterator<Item = Point>, baseline: f64) -> ChartResult<String> {
    let baseline = finite("baseline", baseline)?;
    let points: Vec<Point> = points.into_iter().collect();

    if points.is_empty() {
        return Err(ChartError::EmptySeries);
    }

    let first = points[0];
    let last = points[points.len() - 1];
    let mut output = String::new();
    write_point(
        &mut output,
        "M",
        Point {
            x: first.x,
            y: baseline,
        },
    )?;

    for point in points {
        write_point(&mut output, "L", point)?;
    }

    write_point(
        &mut output,
        "L",
        Point {
            x: last.x,
            y: baseline,
        },
    )?;
    output.push('Z');

    Ok(output)
}

fn write_point(output: &mut String, command: &str, point: Point) -> ChartResult<()> {
    let x = finite("point.x", point.x)?;
    let y = finite("point.y", point.y)?;

    if !output.is_empty() {
        output.push(' ');
    }

    write!(output, "{command}{x},{y}").expect("writing to string should not fail");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_path_emits_svg_commands() {
        let points = [
            Point::new(0.0, 10.0).unwrap(),
            Point::new(20.0, 5.0).unwrap(),
            Point::new(40.0, 15.0).unwrap(),
        ];

        assert_eq!(line_path(points).unwrap(), "M0,10 L20,5 L40,15");
    }

    #[test]
    fn area_path_closes_to_baseline() {
        let points = [
            Point::new(0.0, 10.0).unwrap(),
            Point::new(20.0, 5.0).unwrap(),
        ];

        assert_eq!(
            area_path(points, 30.0).unwrap(),
            "M0,30 L0,10 L20,5 L20,30Z"
        );
    }

    #[test]
    fn path_rejects_empty_series() {
        assert_eq!(line_path([]).unwrap_err(), ChartError::EmptySeries);
    }
}
