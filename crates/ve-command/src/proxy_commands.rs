//! Commands for proxies: recording a built one, and cutting through them.
//!
//! Neither of these changes a frame of the cut, which makes them look like
//! settings rather than edits. They go through the history anyway, because the
//! standing rule is that nothing mutates a project outside a command — and
//! because the rule earns its keep here. Attaching a proxy marks the project
//! dirty, so the reference to a file that was just built on disk is saved
//! rather than lost with the session that built it.

use std::any::Any;

use ve_core::{AssetId, Project, ProxyMedia};

use crate::{Command, CommandError};

/// Records a built proxy against its asset, or removes one.
///
/// Issued by the editor when a proxy build finishes, not by the user directly:
/// the file exists on disk by the time this runs, and this is what makes the
/// project point at it.
#[derive(Debug)]
pub struct SetAssetProxy {
    asset: AssetId,
    proxy: Option<ProxyMedia>,
    /// The proxy that was there before, captured on apply. `Option<Option<_>>`
    /// because "there was no proxy" and "this has not been applied yet" are
    /// different states and undo has to tell them apart.
    previous: Option<Option<ProxyMedia>>,
}

impl SetAssetProxy {
    pub fn attach(asset: AssetId, proxy: ProxyMedia) -> Self {
        SetAssetProxy { asset, proxy: Some(proxy), previous: None }
    }

    /// Forgets an asset's proxy. The file on disk is left alone — this is the
    /// project's reference to it, not the thing itself.
    pub fn detach(asset: AssetId) -> Self {
        SetAssetProxy { asset, proxy: None, previous: None }
    }

    pub fn asset(&self) -> AssetId {
        self.asset
    }
}

impl Command for SetAssetProxy {
    fn name(&self) -> &str {
        if self.proxy.is_some() {
            "Attach Proxy"
        } else {
            "Remove Proxy"
        }
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let asset = project
            .asset_mut(self.asset)
            .ok_or_else(|| CommandError::Rejected(format!("asset {} not found", self.asset)))?;
        let was = asset.proxy.take();
        asset.proxy = self.proxy.clone();
        if self.previous.is_none() {
            self.previous = Some(was);
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("proxy was never set".into()))?;
        let asset = project
            .asset_mut(self.asset)
            .ok_or_else(|| CommandError::Rejected(format!("asset {} not found", self.asset)))?;
        asset.proxy = previous;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Switches decoding through proxies on or off for the whole project.
///
/// One switch rather than one per asset. A cut is made at a resolution, not a
/// file at a time, and an editor that showed some clips at a quarter and others
/// at full size would be lying about what the sequence looks like.
#[derive(Debug)]
pub struct SetUseProxies {
    enabled: bool,
    previous: Option<bool>,
}

impl SetUseProxies {
    pub fn new(enabled: bool) -> Self {
        SetUseProxies { enabled, previous: None }
    }

    /// Whether this would actually change anything, so the caller can decline
    /// to fill the undo stack with a switch that was already in that position.
    pub fn would_change(&self, project: &Project) -> bool {
        project.settings.use_proxies != self.enabled
    }
}

impl Command for SetUseProxies {
    fn name(&self) -> &str {
        if self.enabled {
            "Use Proxies"
        } else {
            "Use Full Resolution"
        }
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        if self.previous.is_none() {
            self.previous = Some(project.settings.use_proxies);
        }
        project.settings.use_proxies = self.enabled;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .ok_or_else(|| CommandError::Rejected("the switch was never moved".into()))?;
        project.settings.use_proxies = previous;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::{MediaInfo, Size};

    fn project_with_asset() -> (Project, AssetId) {
        let mut project = Project::new("Proxies");
        let asset = project.add_asset("/media/movie.mp4", MediaInfo::default());
        (project, asset)
    }

    #[test]
    fn attaching_and_undoing_leaves_the_asset_as_it_was() {
        let (mut project, asset) = project_with_asset();
        let mut command =
            SetAssetProxy::attach(asset, ProxyMedia::new("/p/movie.mov", Size::new(640, 360)));

        command.apply(&mut project).unwrap();
        assert_eq!(
            project.asset(asset).unwrap().proxy.as_ref().unwrap().size,
            Size::new(640, 360)
        );

        command.undo(&mut project).unwrap();
        assert!(!project.asset(asset).unwrap().has_proxy());

        // Redo lands in the same place, which is what the history relies on.
        command.apply(&mut project).unwrap();
        assert!(project.asset(asset).unwrap().has_proxy());
    }

    #[test]
    fn rebuilding_a_proxy_restores_the_one_it_replaced() {
        let (mut project, asset) = project_with_asset();
        project.asset_mut(asset).unwrap().proxy =
            Some(ProxyMedia::new("/p/old.mov", Size::new(320, 180)));

        let mut command =
            SetAssetProxy::attach(asset, ProxyMedia::new("/p/new.mov", Size::new(960, 540)));
        command.apply(&mut project).unwrap();
        command.undo(&mut project).unwrap();

        assert_eq!(
            project.asset(asset).unwrap().proxy.as_ref().unwrap().size,
            Size::new(320, 180),
            "undo must put back the proxy that was replaced, not merely clear one"
        );
    }

    #[test]
    fn detaching_forgets_the_reference_and_undo_brings_it_back() {
        let (mut project, asset) = project_with_asset();
        project.asset_mut(asset).unwrap().proxy =
            Some(ProxyMedia::new("/p/movie.mov", Size::new(640, 360)));

        let mut command = SetAssetProxy::detach(asset);
        command.apply(&mut project).unwrap();
        assert!(!project.asset(asset).unwrap().has_proxy());

        command.undo(&mut project).unwrap();
        assert!(project.asset(asset).unwrap().has_proxy());
    }

    #[test]
    fn the_switch_goes_both_ways_and_knows_when_it_would_do_nothing() {
        let (mut project, _) = project_with_asset();
        assert!(!project.settings.use_proxies);

        assert!(SetUseProxies::new(true).would_change(&project));
        assert!(!SetUseProxies::new(false).would_change(&project));

        let mut command = SetUseProxies::new(true);
        command.apply(&mut project).unwrap();
        assert!(project.settings.use_proxies);
        command.undo(&mut project).unwrap();
        assert!(!project.settings.use_proxies);
    }

    #[test]
    fn attaching_to_an_asset_that_is_gone_is_refused_rather_than_ignored() {
        let (mut project, _) = project_with_asset();
        let mut command = SetAssetProxy::attach(
            AssetId::from_raw(999),
            ProxyMedia::new("/p/x.mov", Size::new(640, 360)),
        );
        assert!(command.apply(&mut project).is_err());
    }
}
