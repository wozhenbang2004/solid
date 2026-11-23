use crate::dom::element::AstDomTransform;
use crate::shared::utils::next_unique_local;

impl AstDomTransform<'_, '_> {
    pub(crate) fn register_get_owner(&mut self) -> String {
        let bindings = &self.bindings;
        self.template_state
            .get_owner_local
            .get_or_insert_with(|| {
                let name = "_$getOwner";
                if bindings.is_taken(name) {
                    let mut index = 0;
                    next_unique_local(name, &mut index, bindings)
                } else {
                    name.to_string()
                }
            })
            .clone()
    }

    pub(crate) fn next_element_id(&mut self) -> String {
        next_unique_local("_el", &mut self.element_index, &self.bindings)
    }

    pub(crate) fn next_ref_id(&mut self) -> String {
        next_unique_local("_ref", &mut self.ref_index, &self.bindings)
    }

    pub(crate) fn next_condition_id(&mut self) -> String {
        next_unique_local("_c", &mut self.condition_index, &self.bindings)
    }

    /// `_o$`, `_o$2`, … — a template root's creation-time owner capture
    /// (Babel's `generateUidIdentifier("o$")`).
    pub(crate) fn next_owner_id(&mut self) -> String {
        next_unique_local("_o", &mut self.owner_index, &self.bindings)
    }
}
