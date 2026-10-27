//! Deterministic, script-free SVG output from the canonical drawing scene.
use super::{CanvasElementKindV1, CanvasElementV1, CanvasSceneV1, CanvasStrokeStyle};
use crate::persistence_service::PersistenceServiceError as Error;
use std::fmt::Write;

fn number(value: f32) -> String {
    let value = if value.abs() < 0.0005 { 0.0 } else { value };
    let formatted = format!("{value:.3}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

pub fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn arrowhead(output: &mut String, point: [f32; 2], from: [f32; 2], color: u32, width: f32) {
    let dx = point[0] - from[0];
    let dy = point[1] - from[1];
    let length = dx.hypot(dy);
    if length < 0.001 {
        return;
    }
    let unit = [dx / length, dy / length];
    let size = (width * 3.5).max(10.0).min(length / 2.0);
    let base = [point[0] - unit[0] * size, point[1] - unit[1] * size];
    let side = size * 0.45;
    let _ = writeln!(
        output,
        "<polygon points=\"{},{} {},{} {},{}\" fill=\"#{color:06x}\"/>",
        number(point[0]),
        number(point[1]),
        number(base[0] - unit[1] * side),
        number(base[1] + unit[0] * side),
        number(base[0] + unit[1] * side),
        number(base[1] - unit[0] * side)
    );
}

fn element_svg(output: &mut String, element: &CanvasElementV1, scene: &CanvasSceneV1) {
    let color = element.stroke_color.rgb(scene.background_mode);
    let fill = if element.fill_enabled {
        format!("#{color:06x}")
    } else {
        "none".to_owned()
    };
    let dash = match element.stroke_style {
        CanvasStrokeStyle::Solid => "",
        CanvasStrokeStyle::Dashed => " stroke-dasharray=\"8 6\"",
        CanvasStrokeStyle::Dotted => " stroke-dasharray=\"1 5\"",
    };
    let style = format!(
        "fill=\"{fill}\" fill-opacity=\"0.18\" stroke=\"#{color:06x}\" stroke-width=\"{}\" stroke-linejoin=\"round\" stroke-linecap=\"round\"{dash}",
        number(element.stroke_width.units())
    );
    let x = number(element.x);
    let y = number(element.y);
    let width = number(element.width);
    let height = number(element.height);
    match &element.shape {
        CanvasElementKindV1::Rectangle { round } => {
            let radius = if *round {
                12.0_f32.min(element.width / 4.0).min(element.height / 4.0)
            } else {
                0.0
            };
            let _ = writeln!(
                output,
                "<rect x=\"{x}\" y=\"{y}\" width=\"{width}\" height=\"{height}\" rx=\"{}\" {style}/>",
                number(radius)
            );
        }
        CanvasElementKindV1::Diamond => {
            let _ = writeln!(
                output,
                "<polygon points=\"{},{} {},{} {},{} {},{}\" {style}/>",
                number(element.x + element.width / 2.0),
                y,
                number(element.x + element.width),
                number(element.y + element.height / 2.0),
                number(element.x + element.width / 2.0),
                number(element.y + element.height),
                x,
                number(element.y + element.height / 2.0)
            );
        }
        CanvasElementKindV1::Ellipse => {
            let _ = writeln!(
                output,
                "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" {style}/>",
                number(element.x + element.width / 2.0),
                number(element.y + element.height / 2.0),
                number(element.width / 2.0),
                number(element.height / 2.0)
            );
        }
        CanvasElementKindV1::Arrow { .. } | CanvasElementKindV1::Line { .. } => {
            if let Some([start, end]) = super::geometry::connector_world_points(element) {
                let _ = writeln!(
                    output,
                    "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" {style}/>",
                    number(start[0]),
                    number(start[1]),
                    number(end[0]),
                    number(end[1])
                );
                if matches!(element.shape, CanvasElementKindV1::Arrow { .. }) {
                    arrowhead(output, end, start, color, element.stroke_width.units());
                    if matches!(
                        element.shape,
                        CanvasElementKindV1::Arrow {
                            bidirectional: true,
                            ..
                        }
                    ) {
                        arrowhead(output, start, end, color, element.stroke_width.units());
                    }
                }
            }
        }
        CanvasElementKindV1::Text { .. } => {}
    }
    if !element.label.is_empty() {
        let font_size = super::geometry::label_font_size(element);
        let bounds = super::geometry::label_bounds(element);
        let is_text = matches!(element.shape, CanvasElementKindV1::Text { .. });
        let text_x = if is_text {
            element.x
        } else {
            element.x + element.width / 2.0
        };
        let anchor = if is_text { "start" } else { "middle" };
        let _ = writeln!(
            output,
            "<text fill=\"#{color:06x}\" font-family=\"Segoe UI, sans-serif\" font-size=\"{}\" text-anchor=\"{anchor}\" xml:space=\"preserve\">",
            number(font_size)
        );
        for (index, line) in super::geometry::label_lines(element).iter().enumerate() {
            let line_y = bounds.y + font_size + index as f32 * font_size * 1.3;
            let _ = writeln!(
                output,
                "<tspan x=\"{}\" y=\"{}\">{}</tspan>",
                number(text_x),
                number(line_y),
                escape_xml(line)
            );
        }
        output.push_str("</text>\n");
    }
}

pub fn export_svg(scene: &CanvasSceneV1) -> Result<String, Error> {
    let scene = super::normalize_scene(scene)?;
    let bounds = super::geometry::scene_bounds(&scene).expanded(24.0);
    let mut output = String::new();
    let _ = writeln!(
        output,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{} {} {} {}\" width=\"{}\" height=\"{}\">",
        number(bounds.x),
        number(bounds.y),
        number(bounds.width),
        number(bounds.height),
        number(bounds.width),
        number(bounds.height)
    );
    let _ = writeln!(
        output,
        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#{:06x}\"/>",
        number(bounds.x),
        number(bounds.y),
        number(bounds.width),
        number(bounds.height),
        scene.background_mode.rgb()
    );
    for element in super::geometry::paint_order(&scene) {
        element_svg(&mut output, element, &scene);
        if output.len() > super::MAX_SVG_BYTES - 8 {
            return Err(Error::Validation(
                "Drawing SVG exceeds the 16 MiB export limit".into(),
            ));
        }
    }
    output.push_str("</svg>\n");
    Ok(output)
}
