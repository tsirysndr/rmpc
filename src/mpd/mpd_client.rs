use std::{
    ops::{Range, RangeInclusive},
    str::FromStr,
};

use anyhow::Result;
use derive_more::Deref;
use strum::AsRefStr;

use crate::shared::{ext::error::ErrorExt, macros::status_error};

use super::{
    client::Client,
    commands::{
        decoders::Decoders, list::MpdList, list_playlist::FileList, outputs::Outputs, status::OnOffOneshot,
        volume::Bound, IdleEvent, ListFiles, LsInfo, Mounts, Playlist, Song, Status, Update, Volume,
    },
    errors::{ErrorCode, MpdError, MpdFailureResponse},
    proto_client::{ProtoClient, SocketClient},
    version::Version,
};

type MpdResult<T> = Result<T, MpdError>;

#[derive(AsRefStr, Debug)]
#[allow(dead_code)]
pub enum SaveMode {
    #[strum(serialize = "create")]
    Create,
    #[strum(serialize = "append")]
    Append,
    #[strum(serialize = "replace")]
    Replace,
}

pub enum ValueChange {
    Increase(u32),
    Decrease(u32),
    Set(u32),
}

impl FromStr for ValueChange {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            v if v.starts_with('-') => Ok(ValueChange::Decrease(v.trim_start_matches('-').parse()?)),
            v if v.starts_with('+') => Ok(ValueChange::Increase(v.trim_start_matches('+').parse()?)),
            v => Ok(ValueChange::Set(v.parse()?)),
        }
    }
}

impl ValueChange {
    fn to_mpd_str(&self) -> String {
        match self {
            ValueChange::Increase(val) => format!("+{val}"),
            ValueChange::Decrease(val) => format!("-{val}"),
            ValueChange::Set(val) => format!("{val}"),
        }
    }
}

#[allow(dead_code)]
pub trait MpdClient: Sized {
    fn version(&mut self) -> Version;
    fn binary_limit(&mut self, limit: u64) -> MpdResult<()>;
    fn password(&mut self, password: &str) -> MpdResult<()>;
    fn commands(&mut self) -> MpdResult<MpdList>;
    fn update(&mut self, path: Option<&str>) -> MpdResult<Update>;
    fn rescan(&mut self, path: Option<&str>) -> MpdResult<Update>;
    fn idle(&mut self, subsystem: Option<IdleEvent>) -> MpdResult<Vec<IdleEvent>>;
    fn enter_idle(&mut self) -> MpdResult<ProtoClient<'static, '_, Self>>
    where
        Self: SocketClient;
    fn noidle(&mut self) -> MpdResult<()>;
    fn get_volume(&mut self) -> MpdResult<Volume>;
    fn set_volume(&mut self, volume: Volume) -> MpdResult<()>;
    /// Set playback volume relative to current
    fn volume(&mut self, change: ValueChange) -> MpdResult<()>;
    fn get_current_song(&mut self) -> MpdResult<Option<Song>>;
    fn get_status(&mut self) -> MpdResult<Status>;
    // Playback control
    fn pause_toggle(&mut self) -> MpdResult<()>;
    fn pause(&mut self) -> MpdResult<()>;
    fn unpause(&mut self) -> MpdResult<()>;
    fn next(&mut self) -> MpdResult<()>;
    fn prev(&mut self) -> MpdResult<()>;
    fn play_pos(&mut self, pos: usize) -> MpdResult<()>;
    fn play(&mut self) -> MpdResult<()>;
    fn play_id(&mut self, id: u32) -> MpdResult<()>;
    fn stop(&mut self) -> MpdResult<()>;
    fn seek_current(&mut self, value: ValueChange) -> MpdResult<()>;
    fn repeat(&mut self, enabled: bool) -> MpdResult<()>;
    fn random(&mut self, enabled: bool) -> MpdResult<()>;
    fn single(&mut self, single: OnOffOneshot) -> MpdResult<()>;
    fn consume(&mut self, consume: OnOffOneshot) -> MpdResult<()>;
    // Mounts
    fn mount(&mut self, name: &str, path: &str) -> MpdResult<()>;
    fn unmount(&mut self, name: &str) -> MpdResult<()>;
    fn list_mounts(&mut self) -> MpdResult<Mounts>;
    // Current queue
    fn add(&mut self, path: &str) -> MpdResult<()>;
    fn clear(&mut self) -> MpdResult<()>;
    fn delete_id(&mut self, id: u32) -> MpdResult<()>;
    fn delete_from_queue(&mut self, songs: SingleOrRange) -> MpdResult<()>;
    fn playlist_info(&mut self) -> MpdResult<Option<Vec<Song>>>;
    fn find(&mut self, filter: &[Filter<'_>]) -> MpdResult<Vec<Song>>;
    fn search(&mut self, filter: &[Filter<'_>]) -> MpdResult<Vec<Song>>;
    fn move_in_queue(&mut self, from: SingleOrRange, to: QueueMoveTarget) -> MpdResult<()>;
    fn move_id(&mut self, id: u32, to: QueueMoveTarget) -> MpdResult<()>;
    fn find_one(&mut self, filter: &[Filter<'_>]) -> MpdResult<Option<Song>>;
    fn find_add(&mut self, filter: &[Filter<'_>]) -> MpdResult<()>;
    fn search_add(&mut self, filter: &[Filter<'_>]) -> MpdResult<()>;
    fn list_tag(&mut self, tag: Tag, filter: Option<&[Filter<'_>]>) -> MpdResult<MpdList>;
    // Database
    fn lsinfo(&mut self, path: Option<&str>) -> MpdResult<LsInfo>;
    fn list_files(&mut self, path: Option<&str>) -> MpdResult<ListFiles>;
    fn read_picture(&mut self, path: &str) -> MpdResult<Option<Vec<u8>>>;
    fn albumart(&mut self, path: &str) -> MpdResult<Option<Vec<u8>>>;
    // Stored playlists
    fn list_playlists(&mut self) -> MpdResult<Vec<Playlist>>;
    fn list_playlist(&mut self, name: &str) -> MpdResult<FileList>;
    fn list_playlist_info(&mut self, playlist: &str, range: Option<SingleOrRange>) -> MpdResult<Vec<Song>>;
    fn load_playlist(&mut self, name: &str) -> MpdResult<()>;
    fn rename_playlist(&mut self, name: &str, new_name: &str) -> MpdResult<()>;
    fn delete_playlist(&mut self, name: &str) -> MpdResult<()>;
    fn delete_from_playlist(&mut self, playlist_name: &str, songs: &SingleOrRange) -> MpdResult<()>;
    fn move_in_playlist(&mut self, playlist_name: &str, range: &SingleOrRange, target_position: usize)
        -> MpdResult<()>;
    fn add_to_playlist(&mut self, playlist_name: &str, uri: &str, target_position: Option<usize>) -> MpdResult<()>;
    fn save_queue_as_playlist(&mut self, name: &str, mode: Option<SaveMode>) -> MpdResult<()>;
    /// This function first invokes [`Self::albumart`].
    /// If no album art is fonud it invokes [`Self::read_picture`].
    /// If no art is still found, but no errors were encountered, None is returned.
    fn find_album_art(&mut self, path: &str) -> MpdResult<Option<Vec<u8>>>;
    // Outputs
    fn outputs(&mut self) -> MpdResult<Outputs>;
    fn toggle_output(&mut self, id: u32) -> MpdResult<()>;
    fn enable_output(&mut self, id: u32) -> MpdResult<()>;
    fn disable_output(&mut self, id: u32) -> MpdResult<()>;
    // Decoders
    fn decoders(&mut self) -> MpdResult<Decoders>;
}

impl MpdClient for Client<'_> {
    fn version(&mut self) -> Version {
        self.version
    }

    fn binary_limit(&mut self, limit: u64) -> MpdResult<()> {
        self.send(&format!("binarylimit {limit}"))
            .and_then(ProtoClient::read_ok)
    }

    fn password(&mut self, password: &str) -> MpdResult<()> {
        self.send(&format!("password {password}"))
            .and_then(ProtoClient::read_ok)
    }

    fn update(&mut self, path: Option<&str>) -> MpdResult<Update> {
        if let Some(path) = path {
            self.send(&format!("update {path}"))
                .and_then(ProtoClient::read_response)
        } else {
            self.send("update").and_then(ProtoClient::read_response)
        }
    }

    fn rescan(&mut self, path: Option<&str>) -> MpdResult<Update> {
        if let Some(path) = path {
            self.send(&format!("rescan {path}"))
                .and_then(ProtoClient::read_response)
        } else {
            self.send("rescan").and_then(ProtoClient::read_response)
        }
    }

    // Lists commands supported by the MPD server
    fn commands(&mut self) -> MpdResult<MpdList> {
        self.send("commands").and_then(ProtoClient::read_response)
    }

    // Queries
    fn idle(&mut self, subsystem: Option<IdleEvent>) -> MpdResult<Vec<IdleEvent>> {
        if let Some(subsystem) = subsystem {
            self.send(&format!("idle {subsystem}"))
                .and_then(ProtoClient::read_response)
        } else {
            self.send("idle").and_then(ProtoClient::read_response)
        }
    }

    fn enter_idle(&mut self) -> MpdResult<ProtoClient<'static, '_, Self>>
    where
        Self: SocketClient,
    {
        self.send("idle")
    }

    fn noidle(&mut self) -> MpdResult<()> {
        self.send("noidle").and_then(ProtoClient::read_ok)
    }

    fn get_volume(&mut self) -> MpdResult<Volume> {
        if self.version < Version::new(0, 23, 0) {
            Err(MpdError::UnsupportedMpdVersion("getvol can be used since MPD 0.23.0"))
        } else {
            self.send("getvol").and_then(ProtoClient::read_response)
        }
    }

    fn set_volume(&mut self, volume: Volume) -> MpdResult<()> {
        self.send(&format!("setvol {}", volume.value()))
            .and_then(ProtoClient::read_ok)
    }

    fn volume(&mut self, change: ValueChange) -> MpdResult<()> {
        match change {
            ValueChange::Increase(_) | ValueChange::Decrease(_) => self
                .send(&format!("volume {}", change.to_mpd_str()))
                .and_then(ProtoClient::read_ok),
            ValueChange::Set(val) => self.send(&format!("setvol {val}")).and_then(ProtoClient::read_ok),
        }
    }

    fn get_current_song(&mut self) -> MpdResult<Option<Song>> {
        self.send("currentsong").and_then(ProtoClient::read_opt_response)
    }

    fn get_status(&mut self) -> MpdResult<Status> {
        self.send("status").and_then(ProtoClient::read_response)
    }

    // Playback control
    fn pause_toggle(&mut self) -> MpdResult<()> {
        self.send("pause").and_then(ProtoClient::read_ok)
    }

    fn pause(&mut self) -> MpdResult<()> {
        self.send("pause 1").and_then(ProtoClient::read_ok)
    }

    fn unpause(&mut self) -> MpdResult<()> {
        self.send("pause 0").and_then(ProtoClient::read_ok)
    }

    fn next(&mut self) -> MpdResult<()> {
        self.send("next").and_then(ProtoClient::read_ok)
    }

    fn prev(&mut self) -> MpdResult<()> {
        self.send("previous").and_then(ProtoClient::read_ok)
    }

    fn play_pos(&mut self, pos: usize) -> MpdResult<()> {
        self.send(&format!("play {pos}")).and_then(ProtoClient::read_ok)
    }

    fn play(&mut self) -> MpdResult<()> {
        self.send("play").and_then(ProtoClient::read_ok)
    }

    fn play_id(&mut self, id: u32) -> MpdResult<()> {
        self.send(&format!("playid {id}")).and_then(ProtoClient::read_ok)
    }

    fn stop(&mut self) -> MpdResult<()> {
        self.send("stop").and_then(ProtoClient::read_ok)
    }

    fn seek_current(&mut self, value: ValueChange) -> MpdResult<()> {
        self.send(&format!("seekcur {}", value.to_mpd_str()))
            .and_then(ProtoClient::read_ok)
    }

    fn repeat(&mut self, enabled: bool) -> MpdResult<()> {
        self.send(&format!("repeat {}", u8::from(enabled)))
            .and_then(ProtoClient::read_ok)
    }

    fn random(&mut self, enabled: bool) -> MpdResult<()> {
        self.send(&format!("random {}", u8::from(enabled)))
            .and_then(ProtoClient::read_ok)
    }

    fn single(&mut self, single: OnOffOneshot) -> MpdResult<()> {
        self.send(&format!("single {}", single.to_mpd_value()))
            .and_then(ProtoClient::read_ok)
    }

    fn consume(&mut self, consume: OnOffOneshot) -> MpdResult<()> {
        if self.version < Version::new(0, 24, 0) && matches!(consume, OnOffOneshot::Oneshot) {
            Err(MpdError::UnsupportedMpdVersion(
                "consume oneshot can be used since MPD 0.24.0",
            ))
        } else {
            self.send(&format!("consume {}", consume.to_mpd_value()))
                .and_then(ProtoClient::read_ok)
        }
    }

    // Mounts
    fn mount(&mut self, name: &str, path: &str) -> MpdResult<()> {
        self.send(&format!("mount \"{name}\" \"{path}\""))
            .and_then(ProtoClient::read_ok)
    }

    fn unmount(&mut self, name: &str) -> MpdResult<()> {
        self.send(&format!("unmount \"{name}\"")).and_then(ProtoClient::read_ok)
    }

    fn list_mounts(&mut self) -> MpdResult<Mounts> {
        self.send("listmounts").and_then(ProtoClient::read_response)
    }

    // Current queue
    fn add(&mut self, path: &str) -> MpdResult<()> {
        self.send(&format!("add \"{path}\"")).and_then(ProtoClient::read_ok)
    }

    fn clear(&mut self) -> MpdResult<()> {
        self.send("clear").and_then(ProtoClient::read_ok)
    }

    fn delete_id(&mut self, id: u32) -> MpdResult<()> {
        self.send(&format!("deleteid \"{id}\"")).and_then(ProtoClient::read_ok)
    }

    fn delete_from_queue(&mut self, songs: SingleOrRange) -> MpdResult<()> {
        self.send(&format!("delete {}", songs.as_mpd_range()))
            .and_then(ProtoClient::read_ok)
    }

    fn move_id(&mut self, id: u32, to: QueueMoveTarget) -> MpdResult<()> {
        self.send(&format!("moveid \"{id}\" \"{}\"", to.as_mpd_str()))
            .and_then(ProtoClient::read_ok)
    }

    fn move_in_queue(&mut self, from: SingleOrRange, to: QueueMoveTarget) -> MpdResult<()> {
        self.send(&format!("move {} {}", from.as_mpd_range(), to.as_mpd_str()))
            .and_then(ProtoClient::read_ok)
    }

    fn playlist_info(&mut self) -> MpdResult<Option<Vec<Song>>> {
        self.send("playlistinfo").and_then(ProtoClient::read_opt_response)
    }

    /// Search the database for songs matching FILTER
    fn find(&mut self, filter: &[Filter<'_>]) -> MpdResult<Vec<Song>> {
        self.send(&format!("find \"({})\"", filter.to_query_str()))
            .and_then(ProtoClient::read_response)
    }

    /// Search the database for songs matching FILTER (see Filters).
    /// Parameters have the same meaning as for find, except that search is not case sensitive.
    fn search(&mut self, filter: &[Filter<'_>]) -> MpdResult<Vec<Song>> {
        let query = filter.to_query_str();
        let query = query.as_str();
        log::debug!(query; "Searching for songs");
        self.send(&format!("search \"({query})\""))
            .and_then(ProtoClient::read_response)
    }

    /// Search the database for songs matching FILTER (see Filters) AND add them to queue.
    /// Parameters have the same meaning as for find, except that search is not case sensitive.
    fn search_add(&mut self, filter: &[Filter<'_>]) -> MpdResult<()> {
        let query = filter.to_query_str();
        let query = query.as_str();
        log::debug!(query; "Searching for songs and adding them");
        self.send(&format!("searchadd \"({query})\""))
            .and_then(ProtoClient::read_ok)
    }

    fn find_one(&mut self, filter: &[Filter<'_>]) -> MpdResult<Option<Song>> {
        Ok(self
            .send(&format!("find \"({})\"", filter.to_query_str()))
            .and_then(ProtoClient::read_response::<Vec<Song>>)?
            .pop())
    }

    fn find_add(&mut self, filter: &[Filter<'_>]) -> MpdResult<()> {
        self.send(&format!("findadd \"({})\"", filter.to_query_str()))
            .and_then(ProtoClient::read_ok)
    }

    fn list_tag(&mut self, tag: Tag, filter: Option<&[Filter<'_>]>) -> MpdResult<MpdList> {
        self.send(&if let Some(filter) = filter {
            format!("list {} \"({})\"", tag.as_str(), filter.to_query_str())
        } else {
            format!("list {}", tag.as_str())
        })
        .and_then(ProtoClient::read_response)
    }

    // Database
    fn lsinfo(&mut self, path: Option<&str>) -> MpdResult<LsInfo> {
        Ok(if let Some(path) = path {
            self.send(&format!("lsinfo \"{path}\""))
                .and_then(ProtoClient::read_opt_response)?
                .unwrap_or_default()
        } else {
            self.send("lsinfo")
                .and_then(ProtoClient::read_opt_response)?
                .unwrap_or_default()
        })
        //     Ok(self
    }

    fn list_files(&mut self, path: Option<&str>) -> MpdResult<ListFiles> {
        Ok(if let Some(path) = path {
            self.send(&format!("listfiles \"{path}\""))
                .and_then(ProtoClient::read_opt_response)?
                .unwrap_or_default()
        } else {
            self.send("listfiles")
                .and_then(ProtoClient::read_opt_response)?
                .unwrap_or_default()
        })
    }

    // Stored playlists
    fn list_playlists(&mut self) -> MpdResult<Vec<Playlist>> {
        self.send("listplaylists").and_then(ProtoClient::read_response)
    }
    fn list_playlist(&mut self, name: &str) -> MpdResult<FileList> {
        self.send(&format!("listplaylist \"{name}\""))
            .and_then(ProtoClient::read_response)
    }
    fn list_playlist_info(&mut self, playlist: &str, range: Option<SingleOrRange>) -> MpdResult<Vec<Song>> {
        if let Some(range) = range {
            if self.version < Version::new(0, 24, 0) {
                return Err(MpdError::UnsupportedMpdVersion(
                    "listplaylistinfo with range can only be used since MPD 0.24.0",
                ));
            }
            self.send(&format!("listplaylistinfo \"{playlist}\" {}", range.as_mpd_range()))
                .and_then(ProtoClient::read_response)
        } else {
            self.send(&format!("listplaylistinfo \"{playlist}\""))
                .and_then(ProtoClient::read_response)
        }
    }
    fn load_playlist(&mut self, name: &str) -> MpdResult<()> {
        self.send(&format!("load \"{name}\"")).and_then(ProtoClient::read_ok)
    }
    fn delete_playlist(&mut self, name: &str) -> MpdResult<()> {
        self.send(&format!("rm \"{name}\"")).and_then(ProtoClient::read_ok)
    }
    fn delete_from_playlist(&mut self, playlist_name: &str, range: &SingleOrRange) -> MpdResult<()> {
        self.send(&format!("playlistdelete \"{playlist_name}\" {}", range.as_mpd_range()))
            .and_then(ProtoClient::read_ok)
    }
    fn move_in_playlist(
        &mut self,
        playlist_name: &str,
        range: &SingleOrRange,
        target_position: usize,
    ) -> MpdResult<()> {
        self.send(&format!(
            "playlistmove \"{playlist_name}\" {} {target_position}",
            range.as_mpd_range()
        ))
        .and_then(ProtoClient::read_ok)
    }

    fn add_to_playlist(&mut self, playlist_name: &str, uri: &str, target_position: Option<usize>) -> MpdResult<()> {
        match target_position {
            Some(target_position) => self
                .send(&format!(r#"playlistadd "{playlist_name}" "{uri}" {target_position}"#))
                .and_then(ProtoClient::read_ok),
            None => self
                .send(&format!(r#"playlistadd "{playlist_name}" "{uri}""#))
                .and_then(ProtoClient::read_ok),
        }
    }

    fn rename_playlist(&mut self, name: &str, new_name: &str) -> MpdResult<()> {
        self.send(&format!("rename \"{name}\" \"{new_name}\""))
            .and_then(ProtoClient::read_ok)
    }

    fn save_queue_as_playlist(&mut self, name: &str, mode: Option<SaveMode>) -> MpdResult<()> {
        if let Some(mode) = mode {
            if self.version < Version::new(0, 24, 0) {
                return Err(MpdError::UnsupportedMpdVersion(
                    "save mode can be used since MPD 0.24.0",
                ));
            }
            self.send(&format!("save \"{name}\" \"{}\"", mode.as_ref()))
                .and_then(ProtoClient::read_ok)
        } else {
            self.send(&format!("save \"{name}\"")).and_then(ProtoClient::read_ok)
        }
    }

    fn read_picture(&mut self, path: &str) -> MpdResult<Option<Vec<u8>>> {
        self.send(&format!("readpicture \"{path}\" 0"))
            .and_then(ProtoClient::read_bin)
    }

    fn albumart(&mut self, path: &str) -> MpdResult<Option<Vec<u8>>> {
        self.send(&format!("albumart \"{path}\" 0"))
            .and_then(ProtoClient::read_bin)
    }

    fn find_album_art(&mut self, path: &str) -> MpdResult<Option<Vec<u8>>> {
        match self.albumart(path) {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None)
            | Err(MpdError::Mpd(MpdFailureResponse {
                code: ErrorCode::NoExist,
                ..
            })) => match self.read_picture(path) {
                Ok(Some(p)) => Ok(Some(p)),
                Ok(None) => {
                    log::debug!("No album art found, falling back to placeholder image");
                    Ok(None)
                }
                Err(MpdError::Mpd(MpdFailureResponse {
                    code: ErrorCode::NoExist,
                    ..
                })) => {
                    log::debug!("No album art found, falling back to placeholder image");
                    Ok(None)
                }
                Err(e) => {
                    status_error!(error:? = e; "Failed to read picture. {}", e.to_status());
                    Ok(None)
                }
            },
            Err(e) => {
                status_error!(error:? = e; "Failed to read picture. {}", e.to_status());
                Ok(None)
            }
        }
    }

    // Outputs
    fn outputs(&mut self) -> MpdResult<Outputs> {
        self.send("outputs").and_then(ProtoClient::read_response)
    }

    fn toggle_output(&mut self, id: u32) -> MpdResult<()> {
        self.send(&format!("toggleoutput {id}")).and_then(ProtoClient::read_ok)
    }

    fn enable_output(&mut self, id: u32) -> MpdResult<()> {
        self.send(&format!("enableoutput {id}")).and_then(ProtoClient::read_ok)
    }

    fn disable_output(&mut self, id: u32) -> MpdResult<()> {
        self.send(&format!("disableoutput {id}")).and_then(ProtoClient::read_ok)
    }

    // Decoders
    fn decoders(&mut self) -> MpdResult<Decoders> {
        self.send("decoders").and_then(ProtoClient::read_response)
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum QueueMoveTarget {
    /// relative to the currently playing song; e.g. +0 moves to right after the current song
    RelativeAdd(usize),
    /// relative to the currently playing song; e.g. -0 moves to right before the current song
    RelativeSub(usize),
    Absolute(usize),
}

impl QueueMoveTarget {
    fn as_mpd_str(&self) -> String {
        match self {
            QueueMoveTarget::RelativeAdd(v) => format!("+{v}"),
            QueueMoveTarget::RelativeSub(v) => format!("-{v}"),
            QueueMoveTarget::Absolute(v) => format!("{v}"),
        }
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct SingleOrRange {
    pub start: usize,
    pub end: Option<usize>,
}

impl From<RangeInclusive<usize>> for SingleOrRange {
    fn from(value: RangeInclusive<usize>) -> Self {
        Self::range(*value.start(), value.end() + 1)
    }
}

impl From<Range<usize>> for SingleOrRange {
    fn from(value: Range<usize>) -> Self {
        Self::range(value.start, value.end)
    }
}

#[derive(Deref)]
pub struct Ranges(Vec<SingleOrRange>);

#[allow(dead_code)]
impl SingleOrRange {
    pub fn single(idx: usize) -> Self {
        Self { start: idx, end: None }
    }
    pub fn range(start: usize, end: usize) -> Self {
        Self { start, end: Some(end) }
    }
    pub fn as_mpd_range(&self) -> String {
        if let Some(end) = self.end {
            format!("\"{}:{}\"", self.start, end)
        } else {
            format!("\"{}\"", self.start)
        }
    }
}

trait StrExt {
    fn escape(self) -> String;
}
impl StrExt for &str {
    fn escape(self) -> String {
        self.replace('\\', r"\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
            .replace('\'', "\\\\'")
            .replace('\"', "\\\"")
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[allow(unused)]
pub enum Tag {
    Any,
    Artist,
    AlbumArtist,
    Album,
    Title,
    File,
    Genre,
    Custom(&'static str),
}

impl Tag {
    fn as_str(&self) -> &str {
        match self {
            Tag::Any => "Any",
            Tag::Artist => "Artist",
            Tag::AlbumArtist => "AlbumArtist",
            Tag::Album => "Album",
            Tag::Title => "Title",
            Tag::File => "File",
            Tag::Genre => "Genre",
            Tag::Custom(v) => v,
        }
    }
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum FilterKind {
    Exact,
    StartsWith,
    #[default]
    Contains,
    Regex,
}

#[derive(Debug)]
pub struct Filter<'value> {
    pub tag: Tag,
    pub value: &'value str,
    pub kind: FilterKind,
}

impl From<&'static str> for Tag {
    fn from(value: &'static str) -> Self {
        Self::Custom(value)
    }
}

#[allow(dead_code)]
impl<'value> Filter<'value> {
    pub fn new<T: Into<Tag>>(tag: T, value: &'value str) -> Self {
        Self {
            tag: tag.into(),
            value,
            kind: FilterKind::Exact,
        }
    }

    pub fn new_with_kind<T: Into<Tag>>(tag: T, value: &'value str, kind: FilterKind) -> Self {
        Self {
            tag: tag.into(),
            value,
            kind,
        }
    }

    pub fn with_type(mut self, t: FilterKind) -> Self {
        self.kind = t;
        self
    }

    fn to_query_str(&self) -> String {
        match self.kind {
            FilterKind::Exact => format!("{} == '{}'", self.tag.as_str(), self.value.escape()),
            FilterKind::StartsWith => format!("{} =~ '^{}'", self.tag.as_str(), self.value.escape()),
            FilterKind::Contains => format!("{} =~ '.*{}.*'", self.tag.as_str(), self.value.escape()),
            FilterKind::Regex => format!("{} =~ '{}'", self.tag.as_str(), self.value.escape()),
        }
    }
}

trait FilterExt {
    fn to_query_str(&self) -> String;
}
impl FilterExt for &[Filter<'_>] {
    fn to_query_str(&self) -> String {
        self.iter().enumerate().fold(String::new(), |mut acc, (idx, filter)| {
            if idx > 0 {
                acc.push_str(&format!(" AND ({})", filter.to_query_str()));
            } else {
                acc.push_str(&format!("({})", filter.to_query_str()));
            }
            acc
        })
    }
}

#[cfg(test)]
mod strext_tests {
    use crate::mpd::mpd_client::StrExt;

    #[test]
    fn escapes_correctly() {
        let input: &'static str = r#"(Artist == "foo'bar")"#;

        assert_eq!(input.escape(), r#"\(Artist == \"foo\\'bar\"\)"#);
    }
}

#[cfg(test)]
mod filter_tests {
    use crate::mpd::mpd_client::{FilterExt, FilterKind, Tag};

    use super::Filter;
    use test_case::test_case;

    #[test_case(Tag::Artist, "Artist")]
    #[test_case(Tag::Album, "Album")]
    #[test_case(Tag::AlbumArtist, "AlbumArtist")]
    #[test_case(Tag::Title, "Title")]
    #[test_case(Tag::File, "File")]
    #[test_case(Tag::Genre, "Genre")]
    #[test_case(Tag::Custom("customtag"), "customtag")]
    fn single_value(tag: Tag, expected: &str) {
        let input: &[Filter<'_>] = &[Filter::new(tag, "mrs singer")];

        assert_eq!(input.to_query_str(), format!("({expected} == 'mrs singer')"));
    }

    #[test]
    fn starts_with() {
        let input: &[Filter<'_>] = &[Filter::new_with_kind(Tag::Artist, "mrs singer", FilterKind::StartsWith)];

        assert_eq!(input.to_query_str(), "(Artist =~ '^mrs singer')");
    }

    #[test]
    fn exact() {
        let input: &[Filter<'_>] = &[Filter::new_with_kind(Tag::Album, "the greatest", FilterKind::Exact)];

        assert_eq!(input.to_query_str(), "(Album == 'the greatest')");
    }

    #[test]
    fn contains() {
        let input: &[Filter<'_>] = &[Filter::new_with_kind(Tag::Album, "the greatest", FilterKind::Contains)];

        assert_eq!(input.to_query_str(), "(Album =~ '.*the greatest.*')");
    }

    #[test]
    fn regex() {
        let input: &[Filter<'_>] = &[Filter::new_with_kind(
            Tag::Album,
            r"the greatest.*\s+[A-Za-z]+$",
            FilterKind::Regex,
        )];

        assert_eq!(input.to_query_str(), r"(Album =~ 'the greatest.*\\\\s+[A-Za-z]+$')");
    }

    #[test]
    fn multiple_values() {
        let input: &[Filter<'_>] = &[
            Filter::new(Tag::Album, "the greatest"),
            Filter::new(Tag::Artist, "mrs singer"),
        ];

        assert_eq!(
            input.to_query_str(),
            "(Album == 'the greatest') AND (Artist == 'mrs singer')"
        );
    }
}
