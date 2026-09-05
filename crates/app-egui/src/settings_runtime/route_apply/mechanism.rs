//! Truthful mapping checked setting contracts в coarse app apply report.

use fastiplayer_settings::{AppRuntimeRoute, SettingApplyMechanism, setting_application_contract};
use settings_core::{ApplyMechanism, SettingId, SettingsError, SettingsResult};

/// Выводит MediaService report mechanism только из checked contract matrix.
///
/// MediaService поддерживает два осмысленных режима: policy становится видимой
/// следующему natural event либо active source контролируемо перестраивается.
/// Unknown ID, contract другого owner-а и новый механизм без явного app mapping
/// останавливают planning, а не превращаются в ложный `InPlace` report.
pub(super) fn media_service_apply_mechanism(
    affected_settings: &[SettingId],
) -> SettingsResult<ApplyMechanism> {
    if affected_settings.is_empty() {
        return Err(SettingsError::access_failed(
            "MediaService route не содержит affected settings",
        ));
    }

    let mut source_rebuild_required = false;
    for setting_id in affected_settings {
        let contract = setting_application_contract(setting_id).ok_or_else(|| {
            SettingsError::AccessFailed {
                id: Some(setting_id.clone()),
                message: format!(
                    "MediaService setting `{}` не имеет checked application contract",
                    setting_id.as_str()
                ),
            }
        })?;
        if contract.route != AppRuntimeRoute::MediaService {
            return Err(SettingsError::AccessFailed {
                id: Some(setting_id.clone()),
                message: format!(
                    "setting `{}` принадлежит {:?}, а не MediaService route",
                    setting_id.as_str(),
                    contract.route
                ),
            });
        }

        match contract.mechanism {
            SettingApplyMechanism::PolicyUpdateInPlace => {}
            SettingApplyMechanism::MediaSourceRebuild => source_rebuild_required = true,
            incompatible => {
                return Err(SettingsError::AccessFailed {
                    id: Some(setting_id.clone()),
                    message: format!(
                        "MediaService setting `{}` использует неподдерживаемый app-report mechanism {incompatible:?}",
                        setting_id.as_str()
                    ),
                });
            }
        }
    }

    Ok(if source_rebuild_required {
        ApplyMechanism::PipelineRebuild
    } else {
        ApplyMechanism::InPlace
    })
}
