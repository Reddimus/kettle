//! Lower the pinned diagram renderer's CSS with the final SVG engine's cascade.

use std::cell::Cell;
use std::rc::Rc;

use kettle_media::{FailureCode, MAX_SVG_WORK};
use roxmltree::Node;

use super::{css, sanitize};

struct Declaration {
    applied: css::Applied,
    important: bool,
}

struct Rule<'a> {
    selector: simplecss::Selector<'a>,
    declarations: Vec<Declaration>,
}

pub(super) struct StyleSheets<'a> {
    rules: Vec<Rule<'a>>,
    remaining: Cell<Option<u64>>,
}

fn charge(remaining: &Cell<Option<u64>>, work: usize) -> bool {
    let next = remaining
        .get()
        .and_then(|left| left.checked_sub(work as u64));
    remaining.set(next);
    next.is_some()
}

fn declarations(text: &str) -> Result<Vec<Declaration>, FailureCode> {
    let mut out = Vec::new();
    for declaration in simplecss::DeclarationTokenizer::from(text) {
        if !css::presentation(declaration.name) && !matches!(declaration.name, "font" | "marker") {
            continue;
        }
        // A relative font size is kept as written, for the writer to resolve
        // against the size the element inherits; only what it resolves is
        // ever written.
        if declaration.name == "font-size" && sanitize::relative_font_size(declaration.value) {
            out.push(Declaration {
                applied: (Rc::from("font-size"), Rc::from(declaration.value.trim())),
                important: declaration.important,
            });
            continue;
        }
        let body = format!("{}:{}", declaration.name, declaration.value);
        // A declaration the checker cannot read is left out, as the engine
        // leaves out one it cannot read; only checked values are written.
        // One that would cost too much still fails the job.
        let checked = match css::declarations(&body) {
            Ok(checked) => checked,
            Err(FailureCode::RenderParse) => continue,
            Err(failure) => return Err(failure),
        };
        for (name, value) in checked {
            out.push(Declaration {
                applied: (Rc::from(name), Rc::from(value)),
                important: declaration.important,
            });
        }
    }
    Ok(out)
}

impl<'a> StyleSheets<'a> {
    pub(super) fn parse(text: &'a str) -> Result<Self, FailureCode> {
        let mut rules = Vec::new();
        for rule in simplecss::StyleSheet::parse(text).rules {
            // Bound matcher recursion even before its element callbacks run.
            if rule.selector.to_string().len() > 256 {
                return Err(FailureCode::RenderResource);
            }
            let mut applied = Vec::new();
            for declaration in rule.declarations {
                let body = format!("{}:{}", declaration.name, declaration.value);
                for mut checked in declarations(&body)? {
                    checked.important = declaration.important;
                    applied.push(checked);
                }
            }
            rules.push(Rule {
                selector: rule.selector,
                declarations: applied,
            });
        }
        Ok(Self {
            rules,
            remaining: Cell::new(Some(10 * MAX_SVG_WORK)),
        })
    }

    pub(super) fn applied(&self, node: Node<'_, '_>) -> Result<Vec<css::Applied>, FailureCode> {
        let mut winners: Vec<(css::Applied, bool)> = Vec::new();
        let mut apply = |declaration: &Declaration| -> Result<(), FailureCode> {
            let (name, value) = &declaration.applied;
            if !charge(
                &self.remaining,
                1 + name.len() * (winners.len() + 1) + value.len(),
            ) {
                return Err(FailureCode::RenderResource);
            }
            if let Some((previous, important)) = winners
                .iter_mut()
                .find(|((property, _), _)| property == name)
            {
                // Match pinned usvg: its first important value retains precedence
                // over later rules and inline declarations, including important ones.
                if !*important {
                    *previous = declaration.applied.clone();
                    *important = declaration.important;
                }
            } else {
                winners.push((declaration.applied.clone(), declaration.important));
            }
            Ok(())
        };
        for rule in &self.rules {
            if !charge(&self.remaining, 1) {
                return Err(FailureCode::RenderResource);
            }
            if rule.selector.matches(&Element(node, &self.remaining)) {
                for declaration in &rule.declarations {
                    apply(declaration)?;
                }
            }
            if self.remaining.get().is_none() {
                return Err(FailureCode::RenderResource);
            }
        }
        if let Some(style) = sanitize::plain(node, "style") {
            for declaration in declarations(style)? {
                apply(&declaration)?;
            }
        }
        Ok(winners.into_iter().map(|(applied, _)| applied).collect())
    }
}

struct Element<'a, 'input>(Node<'a, 'input>, &'a Cell<Option<u64>>);

impl simplecss::Element for Element<'_, '_> {
    fn parent_element(&self) -> Option<Self> {
        charge(self.1, 1).then(|| self.0.parent_element().map(|node| Self(node, self.1)))?
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        charge(self.1, 1).then(|| self.0.prev_sibling_element().map(|node| Self(node, self.1)))?
    }

    fn has_local_name(&self, name: &str) -> bool {
        charge(self.1, 1 + name.len()) && self.0.tag_name().name() == name
    }

    fn attribute_matches(&self, name: &str, operator: simplecss::AttributeOperator<'_>) -> bool {
        let value = self.0.attribute(name);
        charge(self.1, 1 + name.len() + value.map_or(0, str::len))
            && value.is_some_and(|value| operator.matches(value))
    }

    fn pseudo_class_matches(&self, class: simplecss::PseudoClass<'_>) -> bool {
        charge(self.1, 1)
            && matches!(class, simplecss::PseudoClass::FirstChild)
            && self.prev_sibling_element().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_descendant_styles_and_inline_values_become_checked_attributes() {
        let document = sanitize::parse(
            "<svg xmlns='http://www.w3.org/2000/svg' id='diagram'><style>#diagram .node rect{fill:red;stroke:blue}.node[data-look='neo'] rect{stroke:green}</style><g class='node' data-look='neo'><rect id='shape' style='fill:orange'/></g></svg>",
        ).unwrap();
        let lowered = sanitize::write_generated(&document).unwrap();
        let lowered = sanitize::parse(&lowered).unwrap();
        let shape = lowered
            .descendants()
            .find(|node| node.attribute("id") == Some("shape"))
            .unwrap();
        assert_eq!(shape.attribute("fill"), Some("orange"));
        assert_eq!(shape.attribute("stroke"), Some("blue"));
        assert!(!lowered.descendants().any(|node| node.has_tag_name("style")));
        assert_eq!(sanitize::write(&document), Err(FailureCode::RenderParse));
    }

    #[test]
    fn generated_important_priority_matches_the_pinned_svg_engine() {
        let document = sanitize::parse(
            "<svg xmlns='http://www.w3.org/2000/svg'><style>.node{fill:red!important}.node{fill:blue!important}</style><rect class='node' style='fill:green!important'/></svg>",
        ).unwrap();
        let lowered = sanitize::write_generated(&document).unwrap();
        let lowered = sanitize::parse(&lowered).unwrap();
        let shape = lowered
            .descendants()
            .find(|node| node.has_tag_name("rect"))
            .unwrap();
        assert_eq!(shape.attribute("fill"), Some("red"));
    }

    #[test]
    fn a_spent_generated_matcher_cannot_continue_or_accept_partial_styles() {
        let document =
            sanitize::parse("<svg xmlns='http://www.w3.org/2000/svg'><rect class='node'/></svg>")
                .unwrap();
        let sheets = StyleSheets::parse("svg .node{fill:red}").unwrap();
        sheets.remaining.set(Some(2));
        let node = document
            .descendants()
            .find(|node| node.has_tag_name("rect"))
            .unwrap();
        assert_eq!(sheets.applied(node), Err(FailureCode::RenderResource));
        assert_eq!(sheets.applied(node), Err(FailureCode::RenderResource));
    }
}
