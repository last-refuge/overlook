use super::*;
use crate::contact_cards::{contact_card_conn, contact_create_target, contacts_connection_conn};

impl McpWriteTxn<'_, '_> {
    pub fn find_contact(&self, matches: impl FnMut(&str) -> bool) -> Result<Option<Contact>> {
        let Some(id) = find_id(self.tx, "people", self.account, matches)? else {
            return Ok(None);
        };
        Ok(contacts_where_conn(self.tx, self.account, "AND id=?2", &[&id])?.pop())
    }

    pub fn contacts_connection(&self) -> Result<Option<ContactsConnection>> {
        contacts_connection_conn(self.tx, self.account)
    }

    pub fn contact_book(&self, contact_id: &str) -> Result<Option<String>> {
        if let Some(target) = contact_create_target(self.tx, self.account, contact_id)? {
            return Ok(Some(target));
        }
        Ok(contact_card_conn(self.tx, self.account, contact_id)?.map(|card| card.address_book))
    }

    pub fn save_mcp_contact(
        &self,
        contact: &mut Contact,
        queue: bool,
        target_book: Option<&str>,
    ) -> Result<Option<OperationId>> {
        Ok(
            save_contact_edit_tx(self.tx, self.account, contact, queue, target_book)?
                .map(|op| op.id),
        )
    }

    pub fn delete_mcp_contact(
        &self,
        contact: &Contact,
        queue: bool,
    ) -> Result<Option<OperationId>> {
        Ok(delete_contact_edit_tx(self.tx, self.account, contact, queue)?.map(|op| op.id))
    }
}
