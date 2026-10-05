use hotline_proto::types::UserChangeOptions;
use hotline_proto::{Field, FieldId};

/// This user's name, icon, and options, as [`myTran_Agreed`] (121) and
/// [`myTran_SetClientUserInfo`] (304) send them.
///
/// [`myTran_Agreed`]: hotline_proto::Opcode::Agreed
/// [`myTran_SetClientUserInfo`]: hotline_proto::Opcode::SetClientUserInfo
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientUserInfo {
    /// The name shown in the user list.
    pub name: Box<str>,
    /// The icon ID.
    pub icon: u16,
    /// The options.
    pub options: UserChangeOptions,
    /// The reply to a private message, sent with
    /// [`UserChangeOptions::AUTO_RESPONSE`].
    pub automatic_response: Box<str>,
}

impl ClientUserInfo {
    /// The fields of the request. [`myField_Options`] (113) is always sent.
    ///
    /// [`myField_Options`]: hotline_proto::FieldId::Options
    pub(crate) fn fields(&self) -> Vec<Field> {
        let mut fields = vec![
            Field::string(FieldId::UserName, &*self.name),
            Field::u16(FieldId::UserIconID, self.icon),
            Field::u16(FieldId::Options, self.options.bits()),
        ];
        if self.options.contains(UserChangeOptions::AUTO_RESPONSE) {
            fields.push(Field::string(
                FieldId::AutomaticResponse,
                &*self.automatic_response,
            ));
        }
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_send_the_options_when_they_are_empty() {
        let info = ClientUserInfo {
            name: "RustHL".into(),
            ..ClientUserInfo::default()
        };
        assert_eq!(
            info.fields(),
            [
                Field::string(FieldId::UserName, "RustHL"),
                Field::u16(FieldId::UserIconID, 0),
                Field::u16(FieldId::Options, 0),
            ]
        );
    }

    #[test]
    fn fields_send_the_automatic_response_with_its_option() {
        let info = ClientUserInfo {
            name: "RustHL".into(),
            icon: 128,
            options: UserChangeOptions::AUTO_RESPONSE,
            automatic_response: "away".into(),
        };
        assert_eq!(
            info.fields().last(),
            Some(&Field::string(FieldId::AutomaticResponse, "away"))
        );
    }
}
