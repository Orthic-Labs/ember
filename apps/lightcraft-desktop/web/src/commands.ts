import { toggledSection } from './controlSections';
import type { Command } from '@rightkit/react-shell';
import type { CommandInfo, JsonObject, UiState } from './types';
import { translate, type LocaleCode } from './i18n';

export interface UiCommandMeta { id: string; label: string; shortcut: string | null; menu: string[] }

/* Kept in source order with crates/ui-egui/src/menus.rs. Rust snapshot metadata wins when present. */
export const UI_COMMAND_IDS = [
  'app.language.english','app.language.simplifiedChinese','app.language.traditionalChinese','app.language.japanese','app.language.portuguese',
  'view.photoGrid','view.squareGrid','view.gridToggle','tool.guidedUpright','view.detail','view.compare','view.survey','view.people','view.faceBoxes','view.reference','photo.setReference','compare.swap','compare.makeSelect','view.autoAdvance','view.filmstrip','view.leftPanel','view.beforeAfter','view.beforeAfterSplit','view.beforeAfterTopBottom','view.beforeAfterSplitTopBottom','view.showOriginal','view.zoomFit','view.zoom100','view.zoomToggle','view.clickZoom','view.zoomIn','view.zoomOut','view.clipping','view.softProof','view.histogram','view.maskOverlay','view.maskOverlayMode','view.maskOverlayColor','view.maskPins','view.visualizeSpots','view.cropOverlay','view.cropOverlayOrientation','view.back','tool.done','view.filterBar','local.addRoot','local.hide','local.restoreHidden','view.fullScreenPreview','view.enterFullScreen','view.infoOverlay','view.navigator',
  'panel.edit','panel.profiles','panel.crop','panel.remove','panel.masking','panel.redeye','panel.presets','panel.info','panel.keywords','panel.versions','panel.activity','panel.close','section.light','section.color','section.effects','section.detail','section.optics','tool.brush','tool.linear','tool.radial','tool.wbPicker','tool.none','brush.smaller','brush.larger','brush.featherLess','brush.featherMore','dialog.newAlbum','dialog.newFolder','dialog.smartAlbum','view.photoCounts','view.slideshow','view.secondWindow','tool.keywordPainter','view.gridInfo','dialog.allMetadata','dialog.newSmartAlbum','dialog.createPreset','dialog.autoStack','dialog.copySettings','dialog.pasteSettings','view.focusSearch','edit.selectOnlyActive','dialog.export','photo.editInExternal','dialog.mergeHdr','dialog.mergePanorama','dialog.mergeHdrPanorama','merge.hdrLast','merge.panoramaLast','merge.hdrPanoramaLast','file.addPhotos','file.addFolder','file.importLightroom','file.addFromDevice','file.findMissing','file.backupLibrary','file.restoreLibrary','photo.locate','dialog.saveMetadataPreset','app.quit','file.importPresets','file.exportPresets','file.importCurvePresets','file.exportCurvePresets','app.settings','app.openLibrary','app.about','app.systemInfo','app.whatsNew','dialog.cull','app.help','app.discord','app.feedback','app.website','app.github','app.artcraft','app.shortcuts','app.export','app.showInFinder','dialog.rename','dialog.labelNames','dialog.captureTime','photo.tagFromTracklog','app.exportPrevious',
] as const;

const uiLabels: Record<string, string> = {
  'view.photoGrid':'Photo Grid','view.squareGrid':'Square Grid','view.gridToggle':'Grid','view.detail':'Detail','view.compare':'Compare','view.survey':'Survey','view.people':'People','view.reference':'Reference View','view.filmstrip':'Filmstrip','view.beforeAfter':'Compare Before and After','view.beforeAfterSplit':'Before/After Split','view.beforeAfterTopBottom':'Before/After Top/Bottom','view.beforeAfterSplitTopBottom':'Before/After Split Top/Bottom','view.showOriginal':'Show Original','view.zoomFit':'Zoom to Fit','view.zoom100':'Zoom 100%','view.zoomToggle':'Toggle Zoom','view.zoomIn':'Zoom In','view.zoomOut':'Zoom Out','view.clipping':'Show Clipping','view.softProof':'Soft Proofing','view.histogram':'Histogram','view.maskOverlay':'Show Mask Overlay','view.maskPins':'Show Mask Pins','view.filterBar':'Filter Bar','view.navigator':'Navigator','view.fullScreenPreview':'Full Screen Preview','view.enterFullScreen':'Enter Full Screen','view.infoOverlay':'Cycle Info Overlay','view.slideshow':'Slideshow','view.secondWindow':'Second Window','view.focusSearch':'Find…','edit.selectOnlyActive':'Select Only Active Photo','view.back':'Back to Grid',
  'panel.edit':'Edit','panel.profiles':'Profile Browser','panel.crop':'Crop & Rotate','panel.remove':'Remove','panel.masking':'Masking','panel.redeye':'Red Eye','panel.presets':'Presets','panel.info':'Info','panel.keywords':'Keywords','panel.versions':'Versions','panel.activity':'History','panel.close':'Close Panel',
  'tool.brush':'Brush','tool.linear':'Linear Gradient','tool.radial':'Radial Gradient','tool.wbPicker':'White Balance Selector','tool.none':'No Tool','tool.done':'Done','brush.smaller':'Decrease Brush Size','brush.larger':'Increase Brush Size','brush.featherLess':'Decrease Brush Feather','brush.featherMore':'Increase Brush Feather',
  'dialog.newAlbum':'New Album…','dialog.newFolder':'New Folder…','dialog.smartAlbum':'New Smart Album…','dialog.allMetadata':'All Metadata…','dialog.newSmartAlbum':'New Smart Album from Filter…','dialog.createPreset':'Create Preset…','dialog.autoStack':'Auto-Stack by Capture Time…','dialog.copySettings':'Choose Edit Settings to Copy…','dialog.pasteSettings':'Paste Selected Settings…','dialog.export':'Export…','dialog.mergeHdr':'HDR…','dialog.mergePanorama':'Panorama…','dialog.mergeHdrPanorama':'HDR Panorama…','dialog.rename':'Rename Photos…','dialog.labelNames':'Edit Color Label Names…','dialog.captureTime':'Edit Capture Time…','dialog.cull':'Assisted Culling…','dialog.saveMetadataPreset':'Save Metadata Preset…',
  'file.addPhotos':'Import Photos…','file.addFolder':'Import from Folder…','file.importLightroom':'Import LRCAT Catalog…','file.addFromDevice':'Import from Device','file.findMissing':'Find Missing Photos…','file.backupLibrary':'Back Up Library…','file.restoreLibrary':'Restore Library from Backup…','file.importPresets':'Import Profiles & Presets…','file.exportPresets':'Export Presets…','file.importCurvePresets':'Import Point Curve Presets…','file.exportCurvePresets':'Export Point Curve Presets…',
  'app.settings':'Settings…','app.openLibrary':'Open Library…','app.about':'About Ember','app.systemInfo':'System Info…','app.whatsNew':"What's New",'app.help':'Ember Help','app.discord':'Ember Community…','app.feedback':'Send Feedback…','app.website':'Ember Project','app.github':'Ember on GitHub','app.artcraft':'Orthic Labs','app.shortcuts':'Keyboard Shortcuts','app.export':'Export Now','app.showInFinder':'Show in Finder','app.quit':'Quit Ember','app.exportPrevious':'Export with Previous',
  'photo.setReference':'Set as Reference Photo','compare.swap':'Swap Compare Photos','compare.makeSelect':'Make Candidate the Select','view.autoAdvance':'Auto Advance','view.leftPanel':'My Photos Panel','view.clickZoom':'Click Zoom Ratio','view.maskOverlayMode':'Cycle Mask Overlay Mode','view.maskOverlayColor':'Cycle Mask Overlay Color','view.visualizeSpots':'Visualize Spots','view.cropOverlay':'Cycle Crop Overlay','view.cropOverlayOrientation':'Cycle Crop Overlay Orientation','view.photoCounts':'Show Photo Counts','view.gridInfo':'Grid Info','photo.editInExternal':'Edit in External Editor','photo.locate':'Locate Missing File…','photo.tagFromTracklog':'Auto-Tag from Tracklog…','local.addRoot':'Add Folder to Local','local.hide':'Remove from Local','local.restoreHidden':'Show Hidden Local Locations','section.light':'Light','section.color':'Color','section.effects':'Effects','section.detail':'Detail','section.optics':'Optics','tool.guidedUpright':'Guided Upright','tool.keywordPainter':'Keyword Painter','merge.hdrLast':'HDR with Last Settings','merge.panoramaLast':'Panorama with Last Settings','merge.hdrPanoramaLast':'HDR Panorama with Last Settings','app.language.english':'English','app.language.simplifiedChinese':'简体中文','app.language.traditionalChinese':'繁體中文','app.language.japanese':'日本語','app.language.portuguese':'Português (Brasil)',
};

const shortcutById: Record<string, string> = {
  'view.gridToggle':'G','tool.guidedUpright':'Shift+G','view.detail':'D','view.compare':'Shift+C','view.survey':'N','view.reference':'Shift+R','view.filmstrip':'/','view.leftPanel':'Cmd+Shift+L','view.beforeAfter':'Y','view.beforeAfterSplit':'Shift+Y','view.beforeAfterTopBottom':'Alt+Y','view.beforeAfterSplitTopBottom':'Alt+Shift+Y','view.showOriginal':'\\','view.zoomFit':'Cmd+0','view.zoom100':'Cmd+Alt+0','view.zoomToggle':'Z','view.zoomIn':'Cmd+=','view.zoomOut':'Cmd+-','view.clipping':'J','view.softProof':'S','view.histogram':'Cmd+Shift+H','view.maskOverlay':'O','view.visualizeSpots':'A','view.cropOverlay':'Shift+O','view.back':'Escape','tool.done':'Enter','view.filterBar':'Shift+F','view.fullScreenPreview':'F','view.enterFullScreen':'Cmd+Shift+F','view.infoOverlay':'Cmd+I','panel.edit':'E','panel.crop':'C','panel.remove':'H','panel.masking':'M','panel.presets':'Shift+P','panel.info':'I','panel.keywords':'K','panel.versions':'Shift+V','section.light':'Cmd+1','section.color':'Cmd+2','section.effects':'Cmd+3','section.detail':'Cmd+4','section.optics':'Cmd+5','tool.brush':'B','tool.linear':'L','tool.radial':'R','tool.wbPicker':'W','brush.smaller':'[','brush.larger':']','brush.featherLess':'Shift+[','brush.featherMore':'Shift+]','dialog.newAlbum':'Cmd+N','dialog.newFolder':'Cmd+Shift+N','view.slideshow':'Cmd+Alt+Enter','view.secondWindow':'Cmd+F11','dialog.newSmartAlbum':'Cmd+Alt+N','dialog.createPreset':'Cmd+Shift+P','dialog.copySettings':'Cmd+Shift+C','dialog.pasteSettings':'Cmd+Shift+V','view.focusSearch':'Cmd+F','edit.selectOnlyActive':'Cmd+Shift+D','photo.editInExternal':'Cmd+Shift+E','dialog.mergeHdr':'Ctrl+H','dialog.mergePanorama':'Ctrl+M','merge.hdrLast':'Ctrl+Shift+H','merge.panoramaLast':'Ctrl+Shift+M','file.addPhotos':'Cmd+Shift+I','app.quit':'Cmd+Q','app.settings':'Cmd+,','app.help':'F1','app.shortcuts':'Cmd+/','app.showInFinder':'Cmd+R','dialog.rename':'F2','app.exportPrevious':'Cmd+Alt+Shift+E',
};

function labelFor(id: string): string {
  return uiLabels[id] ?? id.split('.').pop()?.replace(/([A-Z])/g, ' $1').replace(/^./, (v) => v.toUpperCase()) ?? id;
}

export function uiMetadata(id: string, locale: LocaleCode = 'en'): UiCommandMeta {
  const raw = shortcutById[id];
  const menu = id.startsWith('view.') ? ['View'] : id.startsWith('panel.') || id.startsWith('section.') || id.startsWith('tool.') ? ['Window'] : id.startsWith('dialog.') || id.startsWith('file.') ? ['File'] : id.startsWith('app.') ? ['Help'] : [];
  return { id, label: translate(locale, labelFor(id)), shortcut: raw ?? null, menu };
}

export function allUiMetadata(locale: LocaleCode = 'en'): UiCommandMeta[] { return UI_COMMAND_IDS.map((id) => uiMetadata(id, locale)); }

export function paletteCommands(snapshot: { commands?: CommandInfo[] } | null, run: (id: string) => Promise<unknown>, locale: LocaleCode = 'en'): Command[] {
  const generated = allUiMetadata(locale).map((entry) => ({ ...entry, params: '{}', enabled: true }));
  const source = [...(snapshot?.commands ?? []), ...generated].filter((entry, index, all) => all.findIndex((candidate) => candidate.id === entry.id) === index);
  return source.map((entry) => ({ id: entry.id, label: translate(locale, entry.label), group: entry.menu?.map((part) => translate(locale, part)).join(' › ') || translate(locale, 'Commands'), chord: entry.shortcut ?? undefined, disabled: !entry.enabled, run: () => { void run(entry.id); } }));
}

export function applyUiCommand(id: string, ui: UiState): Partial<UiState> | null {
  const locale: Record<string, string> = {
    'app.language.english': 'en',
    'app.language.simplifiedChinese': 'zh-hans',
    'app.language.traditionalChinese': 'zh-hant',
    'app.language.japanese': 'ja',
    'app.language.portuguese': 'pt-br',
  };
  if (locale[id]) return { locale: locale[id] };
  const view: Record<string, UiState['view']> = { photoGrid:'photoGrid', squareGrid:'squareGrid', detail:'detail', compare:'compare', survey:'survey', people:'people', reference:'reference' };
  if (id.startsWith('view.') && id.slice(5) in view) return { view: view[id.slice(5)] };
  if (id.startsWith('panel.')) {
    const panel = id.slice(6);
    if (panel === 'close') return { inspectorCollapsed: true };
    if (['edit','profiles','crop','remove','masking','redeye','presets','info','keywords','versions','activity'].includes(panel)) return { panel: panel as UiState['panel'], inspectorCollapsed: false, ...(['edit','profiles','crop','remove','masking','redeye','presets'].includes(panel) ? { view: 'detail' } : {}) };
  }
  if (id.startsWith('section.')) return { view: 'detail', panel: 'edit', inspectorCollapsed: false, sections: toggledSection(ui.sections, id.slice(8)) };
  if (id.startsWith('tool.')) {
    const tool = id.slice(5);
    if (tool === 'none' || tool === 'done') return { tool: '' };
    if (tool === 'guidedUpright') return { view: 'detail', panel: 'crop', inspectorCollapsed: false, tool };
    if (tool === 'wbPicker') return { view: 'detail', panel: 'edit', inspectorCollapsed: false, tool };
    if (['brush', 'linear', 'radial'].includes(tool)) return { view: 'detail', panel: 'masking', inspectorCollapsed: false, tool };
    if (tool === 'keywordPainter') return { view: 'photoGrid', tool };
    return { tool };
  }
  if (id === 'view.gridToggle') return { view: ui.view === 'squareGrid' ? 'photoGrid' : 'squareGrid' };
  if (id === 'view.gridInfo') return { gridInfo: !ui.gridInfo };
  if (id === 'view.faceBoxes') return { sections: { ...ui.sections, faceBoxes: !ui.sections.faceBoxes } };
  if (id === 'view.photoCounts') return { sections: { ...ui.sections, photoCounts: !ui.sections.photoCounts } };
  if (id === 'view.leftPanel') return { sidebarCollapsed: !ui.sidebarCollapsed };
  if (id === 'view.autoAdvance') return { autoAdvance: !ui.autoAdvance };
  if (id === 'view.filmstrip') return { filmstrip: !ui.filmstrip };
  if (id === 'view.filterBar') return { view: 'photoGrid', filterBar: !ui.filterBar };
  if (id === 'view.navigator') return { navigator: !ui.navigator };
  if (id === 'view.clipping') return { clipping: !ui.clipping };
  if (id === 'view.softProof') return { softProof: !ui.softProof };
  if (id === 'view.maskOverlay') return { maskOverlay: !ui.maskOverlay };
  if (id === 'view.maskOverlayMode') {
    const modes = ['selected', 'all', 'off']; const next = modes[(modes.indexOf(ui.maskOverlayMode) + 1) % modes.length];
    return { maskOverlay: next !== 'off', maskOverlayMode: next };
  }
  if (id === 'view.maskOverlayColor') return { sections: { ...ui.sections, maskOverlayColor: !ui.sections.maskOverlayColor } };
  if (id === 'view.maskPins') return { maskPins: !ui.maskPins };
  if (id === 'view.histogram') return { sections: { ...ui.sections, histogram: !ui.sections.histogram } };
  if (id === 'view.visualizeSpots') return { view: 'detail', panel: 'remove', inspectorCollapsed: false, maskOverlay: true };
  if (id === 'view.cropOverlay') return { cropOverlay: ui.cropOverlay === 'off' ? 'thirds' : 'off' };
  if (id === 'view.cropOverlayOrientation') {
    const orientations = ['thirds', 'golden', 'diagonal', 'off']; const current = orientations.indexOf(ui.cropOverlay);
    return { cropOverlay: orientations[(current + 1 + orientations.length) % orientations.length] };
  }
  if (id === 'view.beforeAfterSplit') return { beforeAfter: ui.beforeAfter === 'split' ? 'off' : 'split' };
  if (id === 'view.beforeAfterTopBottom') return { beforeAfter: ui.beforeAfter === 'topBottom' ? 'off' : 'topBottom' };
  if (id === 'view.beforeAfterSplitTopBottom') return { beforeAfter: ui.beforeAfter === 'splitTopBottom' ? 'off' : 'splitTopBottom' };
  if (id === 'view.beforeAfter') return { beforeAfter: ui.beforeAfter === 'off' ? 'sideBySide' : 'off' };
  if (id === 'view.showOriginal') return { beforeAfter: ui.beforeAfter === 'original' ? 'off' : 'original' };
  if (id === 'view.zoomFit') return { zoom: 'fit' };
  if (id === 'view.zoom100') return { zoom: 1 };
  if (id === 'view.zoomToggle') return { zoom: ui.zoom === 'fit' ? 1 : 'fit' };
  if (id === 'view.clickZoom') return { clickZoom: ui.clickZoom >= 4 ? 1 : ui.clickZoom * 2 };
  if (id === 'view.zoomIn') return { zoom: typeof ui.zoom === 'number' ? Math.min(16, ui.zoom * 1.25) : 1.25 };
  if (id === 'view.zoomOut') return { zoom: typeof ui.zoom === 'number' ? Math.max(0.1, ui.zoom / 1.25) : 0.8 };
  if (id === 'view.infoOverlay') {
    const mode = Number.isFinite(ui.infoOverlay) ? Math.trunc(ui.infoOverlay) : 0;
    return { infoOverlay: (Math.max(0, Math.min(2, mode)) + 1) % 3 };
  }
  if (id === 'view.slideshow') return { slideshow: !ui.slideshow };
  if (id === 'view.fullScreenPreview') return { slideshow: false, zoom: 'fit' };
  if (id === 'view.enterFullScreen') return {};
  if (id === 'view.back') {
    if (ui.tool) return { tool: '' };
    if (ui.view === 'compare' || ui.view === 'survey') return { view: 'detail' };
    if (ui.view === 'detail') return { view: 'photoGrid' };
    return {};
  }
  if (id === 'brush.smaller') return { brushSize: Math.max(2, ui.brushSize * 0.9) };
  if (id === 'brush.larger') return { brushSize: Math.min(500, ui.brushSize * 1.1) };
  if (id === 'brush.featherLess') return { brushFeather: Math.max(0, ui.brushFeather - 5) };
  if (id === 'brush.featherMore') return { brushFeather: Math.min(100, ui.brushFeather + 5) };
  return null;
}
