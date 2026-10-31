use std::collections::HashSet;

use anyhow::ensure;
use rusqlite::{
    Connection, params,
    types::{FromSql, FromSqlError, FromSqlResult, ValueRef},
};

use crate::{
    database,
    hotkeys::Hotkey,
    i18n::{Text, UserContext},
    soundboard::sound::{Sound, SoundId, is_valid_name},
};

#[derive(Clone, Default)]
pub struct Library {
    pub sounds: Vec<Sound>,
}

impl Library {
    pub fn try_load() -> anyhow::Result<Self> {
        Self::load(&database::open()?)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&mut database::open()?)
            .user_context(Text::SaveSoundLibrary)
    }

    fn save_to(&self, connection: &mut Connection) -> anyhow::Result<()> {
        let transaction = connection.transaction()?;
        {
            let mut update = transaction
                .prepare("UPDATE sounds SET name = ?1, position = ?2, hotkey = ?3 WHERE id = ?4")?;
            for (position, sound) in self.sounds.iter().enumerate() {
                ensure!(is_valid_name(&sound.name), Text::InvalidSoundName);
                ensure!(
                    update
                        .execute(params![sound.name, position as i64, sound.hotkey, sound.id,])?
                        == 1,
                    Text::SoundNotFound
                );
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn move_sounds(
        &mut self,
        ids: &HashSet<SoundId>,
        to_top: bool,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        ensure!(
            !ids.is_empty()
                && self
                    .sounds
                    .iter()
                    .filter(|sound| ids.contains(&sound.id))
                    .count()
                    == ids.len(),
            Text::SoundNotFound
        );
        let original = self.sounds.clone();
        self.sounds
            .sort_by_key(|sound| ids.contains(&sound.id) != to_top);
        if let Err(error) = save(self) {
            self.sounds = original;
            return Err(error);
        }
        Ok(())
    }

    pub fn move_sound(
        &mut self,
        from: usize,
        to: usize,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        ensure!(
            from < self.sounds.len() && to < self.sounds.len(),
            Text::SoundNotFound
        );
        if from == to {
            return Ok(());
        }
        let sound = self.sounds.remove(from);
        self.sounds.insert(to, sound);
        if let Err(error) = save(self) {
            let sound = self.sounds.remove(to);
            self.sounds.insert(from, sound);
            return Err(error);
        }
        Ok(())
    }

    pub fn set_hotkeys(
        &mut self,
        ids: &HashSet<SoundId>,
        key: Option<Hotkey>,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let indices: Vec<_> = self
            .sounds
            .iter()
            .enumerate()
            .filter(|(_, sound)| ids.contains(&sound.id))
            .map(|(index, _)| index)
            .collect();
        ensure!(
            !indices.is_empty() && indices.len() == ids.len(),
            Text::SoundNotFound
        );
        let previous: Vec<_> = indices
            .iter()
            .map(|&index| std::mem::replace(&mut self.sounds[index].hotkey, key))
            .collect();
        if let Err(error) = save(self) {
            for (index, key) in indices.into_iter().zip(previous) {
                self.sounds[index].hotkey = key;
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn rename(
        &mut self,
        index: usize,
        name: &str,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let name = name.trim();
        ensure!(is_valid_name(name), Text::InvalidSoundName);
        let sound = self
            .sounds
            .get_mut(index)
            .user_context(Text::SoundNotFound)?;
        let original = std::mem::replace(&mut sound.name, name.to_owned());
        if let Err(error) = save(self) {
            self.sounds[index].name = original;
            return Err(error);
        }
        Ok(())
    }

    pub fn remove(
        &mut self,
        connection: &mut Connection,
        ids: &HashSet<SoundId>,
        reclaim_space: bool,
    ) -> anyhow::Result<()> {
        let transaction = connection.transaction()?;
        {
            let mut delete = transaction.prepare("DELETE FROM sounds WHERE id = ?1")?;
            for id in ids {
                delete.execute([id])?;
            }
        }
        transaction.commit()?;
        self.sounds.retain(|sound| !ids.contains(&sound.id));
        if reclaim_space {
            // Maintenance errors must not turn a committed deletion into a failure.
            if let Err(error) = database::reclaim_space(connection) {
                tracing::warn!("Database space reclamation incomplete: {error:#}");
            }
        } else {
            tracing::info!("Database space reclamation skipped: playback is still active");
        }
        Ok(())
    }

    pub fn load(connection: &Connection) -> anyhow::Result<Self> {
        let mut query = connection
            .prepare("SELECT id, name, duration_secs, hotkey FROM sounds ORDER BY position, id")?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
                read_hotkey(row.get_ref(3)?),
            ))
        })?;
        let mut sounds = Vec::new();
        for row in rows {
            let (id, name, duration_secs, hotkey) = row?;
            ensure!(is_valid_name(&name), Text::InvalidSoundName);
            ensure!(
                duration_secs.is_finite() && duration_secs > 0.0,
                Text::InvalidAudioDuration
            );
            let hotkey = match hotkey {
                Ok(hotkey) => hotkey,
                Err(error) => {
                    tracing::warn!(sound_id = id, %error, "Invalid sound hotkey; clearing binding");
                    None
                }
            };
            sounds.push(Sound {
                id,
                name,
                duration_secs,
                hotkey,
            });
        }
        Ok(Self { sounds })
    }
}

fn read_hotkey(value: ValueRef<'_>) -> FromSqlResult<Option<Hotkey>> {
    let hotkey = Option::<Hotkey>::column_result(value)?;
    match hotkey {
        Some(key) if !key.is_valid() => Err(FromSqlError::Other("invalid sound hotkey".into())),
        _ => Ok(hotkey),
    }
}
