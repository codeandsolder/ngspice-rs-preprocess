// Generated literal-default facts, not C implementation code.
// Source: ngspice 059066b532f7073fccf4173ef269309eafdc7f0c, bsim3/b3set.c.
// Dependent defaults and diagnostics are implemented separately.

#[rustfmt::skip]
macro_rules! apply_literal_defaults {
    ($values:ident) => {
        $values.mobmod.apply_default(1);
        $values.binunit.apply_default(1);
        $values.paramchk.apply_default(0);
        $values.capmod.apply_default(3);
        $values.acm.apply_default(0);
        $values.calcacm.apply_default(0);
        $values.noimod.apply_default(1);
        $values.nqsmod.apply_default(0);
        $values.acnqsmod.apply_default(0);
        $values.tox.apply_default(1.5e-8);
        $values.cdsc.apply_default(0.00024);
        $values.cdscb.apply_default(0.0);
        $values.cdscd.apply_default(0.0);
        $values.cit.apply_default(0.0);
        $values.nfactor.apply_default(1.0);
        $values.xj.apply_default(1.5e-7);
        $values.vsat.apply_default(80000.0);
        $values.at.apply_default(33000.0);
        $values.a0.apply_default(1.0);
        $values.ags.apply_default(0.0);
        $values.a1.apply_default(0.0);
        $values.a2.apply_default(1.0);
        $values.keta.apply_default(-0.047);
        $values.nsub.apply_default(6e16);
        $values.nch.apply_default(1.7e17);
        $values.ngate.apply_default(0.0);
        $values.vbm.apply_default(-3.0);
        $values.xt.apply_default(1.55e-7);
        $values.kt1.apply_default(-0.11);
        $values.kt1l.apply_default(0.0);
        $values.kt2.apply_default(0.022);
        $values.k3.apply_default(80.0);
        $values.k3b.apply_default(0.0);
        $values.w0.apply_default(2.5e-6);
        $values.nlx.apply_default(1.74e-7);
        $values.dvt0.apply_default(2.2);
        $values.dvt1.apply_default(0.53);
        $values.dvt2.apply_default(-0.032);
        $values.dvt0w.apply_default(0.0);
        $values.dvt1w.apply_default(5_300_000.0);
        $values.dvt2w.apply_default(-0.032);
        $values.drout.apply_default(0.56);
        $values.ua.apply_default(2.25e-9);
        $values.ua1.apply_default(4.31e-9);
        $values.ub.apply_default(5.87e-19);
        $values.ub1.apply_default(-7.61e-18);
        $values.ute.apply_default(-1.5);
        $values.voff.apply_default(-0.08);
        $values.delta.apply_default(0.01);
        $values.rdsw.apply_default(0.0);
        $values.prwg.apply_default(0.0);
        $values.prwb.apply_default(0.0);
        $values.prt.apply_default(0.0);
        $values.eta0.apply_default(0.08);
        $values.etab.apply_default(-0.07);
        $values.pclm.apply_default(1.3);
        $values.pdiblc1.apply_default(0.39);
        $values.pdiblc2.apply_default(0.0086);
        $values.pdiblcb.apply_default(0.0);
        $values.pscbe1.apply_default(424_000_000.0);
        $values.pscbe2.apply_default(1e-5);
        $values.pvag.apply_default(0.0);
        $values.wr.apply_default(1.0);
        $values.dwg.apply_default(0.0);
        $values.dwb.apply_default(0.0);
        $values.b0.apply_default(0.0);
        $values.b1.apply_default(0.0);
        $values.alpha0.apply_default(0.0);
        $values.alpha1.apply_default(0.0);
        $values.beta0.apply_default(30.0);
        $values.ijth.apply_default(0.1);
        $values.elm.apply_default(5.0);
        $values.cgsl.apply_default(0.0);
        $values.cgdl.apply_default(0.0);
        $values.ckappa.apply_default(0.6);
        $values.clc.apply_default(1e-7);
        $values.cle.apply_default(0.6);
        $values.vfbcv.apply_default(-1.0);
        $values.acde.apply_default(1.0);
        $values.moin.apply_default(15.0);
        $values.noff.apply_default(1.0);
        $values.voffcv.apply_default(0.0);
        $values.tcj.apply_default(0.0);
        $values.tpb.apply_default(0.0);
        $values.tcjsw.apply_default(0.0);
        $values.tpbsw.apply_default(0.0);
        $values.tcjswg.apply_default(0.0);
        $values.tpbswg.apply_default(0.0);
        $values.hdif.apply_default(0.0);
        $values.ldif.apply_default(0.0);
        $values.ld.apply_default(0.0);
        $values.rd.apply_default(0.0);
        $values.rs.apply_default(0.0);
        $values.rdc.apply_default(0.0);
        $values.rsc.apply_default(0.0);
        $values.wmlt.apply_default(1.0);
        $values.lcdsc.apply_default(0.0);
        $values.lcdscb.apply_default(0.0);
        $values.lcdscd.apply_default(0.0);
        $values.lcit.apply_default(0.0);
        $values.lnfactor.apply_default(0.0);
        $values.lxj.apply_default(0.0);
        $values.lvsat.apply_default(0.0);
        $values.lat.apply_default(0.0);
        $values.la0.apply_default(0.0);
        $values.lags.apply_default(0.0);
        $values.la1.apply_default(0.0);
        $values.la2.apply_default(0.0);
        $values.lketa.apply_default(0.0);
        $values.lnsub.apply_default(0.0);
        $values.lnch.apply_default(0.0);
        $values.lngate.apply_default(0.0);
        $values.lvbm.apply_default(0.0);
        $values.lxt.apply_default(0.0);
        $values.lkt1.apply_default(0.0);
        $values.lkt1l.apply_default(0.0);
        $values.lkt2.apply_default(0.0);
        $values.lk3.apply_default(0.0);
        $values.lk3b.apply_default(0.0);
        $values.lw0.apply_default(0.0);
        $values.lnlx.apply_default(0.0);
        $values.ldvt0.apply_default(0.0);
        $values.ldvt1.apply_default(0.0);
        $values.ldvt2.apply_default(0.0);
        $values.ldvt0w.apply_default(0.0);
        $values.ldvt1w.apply_default(0.0);
        $values.ldvt2w.apply_default(0.0);
        $values.ldrout.apply_default(0.0);
        $values.ldsub.apply_default(0.0);
        $values.lvth0.apply_default(0.0);
        $values.lua.apply_default(0.0);
        $values.lua1.apply_default(0.0);
        $values.lub.apply_default(0.0);
        $values.lub1.apply_default(0.0);
        $values.luc.apply_default(0.0);
        $values.luc1.apply_default(0.0);
        $values.lu0.apply_default(0.0);
        $values.lute.apply_default(0.0);
        $values.lvoff.apply_default(0.0);
        $values.ldelta.apply_default(0.0);
        $values.lrdsw.apply_default(0.0);
        $values.lprwb.apply_default(0.0);
        $values.lprwg.apply_default(0.0);
        $values.lprt.apply_default(0.0);
        $values.leta0.apply_default(0.0);
        $values.letab.apply_default(-0.0);
        $values.lpclm.apply_default(0.0);
        $values.lpdiblc1.apply_default(0.0);
        $values.lpdiblc2.apply_default(0.0);
        $values.lpdiblcb.apply_default(0.0);
        $values.lpscbe1.apply_default(0.0);
        $values.lpscbe2.apply_default(0.0);
        $values.lpvag.apply_default(0.0);
        $values.lwr.apply_default(0.0);
        $values.ldwg.apply_default(0.0);
        $values.ldwb.apply_default(0.0);
        $values.lb0.apply_default(0.0);
        $values.lb1.apply_default(0.0);
        $values.lalpha0.apply_default(0.0);
        $values.lalpha1.apply_default(0.0);
        $values.lbeta0.apply_default(0.0);
        $values.lvfb.apply_default(0.0);
        $values.lelm.apply_default(0.0);
        $values.lcgsl.apply_default(0.0);
        $values.lcgdl.apply_default(0.0);
        $values.lckappa.apply_default(0.0);
        $values.lclc.apply_default(0.0);
        $values.lcle.apply_default(0.0);
        $values.lcf.apply_default(0.0);
        $values.lvfbcv.apply_default(0.0);
        $values.lacde.apply_default(0.0);
        $values.lmoin.apply_default(0.0);
        $values.lnoff.apply_default(0.0);
        $values.lvoffcv.apply_default(0.0);
        $values.wcdsc.apply_default(0.0);
        $values.wcdscb.apply_default(0.0);
        $values.wcdscd.apply_default(0.0);
        $values.wcit.apply_default(0.0);
        $values.wnfactor.apply_default(0.0);
        $values.wxj.apply_default(0.0);
        $values.wvsat.apply_default(0.0);
        $values.wat.apply_default(0.0);
        $values.wa0.apply_default(0.0);
        $values.wags.apply_default(0.0);
        $values.wa1.apply_default(0.0);
        $values.wa2.apply_default(0.0);
        $values.wketa.apply_default(0.0);
        $values.wnsub.apply_default(0.0);
        $values.wnch.apply_default(0.0);
        $values.wngate.apply_default(0.0);
        $values.wvbm.apply_default(0.0);
        $values.wxt.apply_default(0.0);
        $values.wkt1.apply_default(0.0);
        $values.wkt1l.apply_default(0.0);
        $values.wkt2.apply_default(0.0);
        $values.wk3.apply_default(0.0);
        $values.wk3b.apply_default(0.0);
        $values.ww0.apply_default(0.0);
        $values.wnlx.apply_default(0.0);
        $values.wdvt0.apply_default(0.0);
        $values.wdvt1.apply_default(0.0);
        $values.wdvt2.apply_default(0.0);
        $values.wdvt0w.apply_default(0.0);
        $values.wdvt1w.apply_default(0.0);
        $values.wdvt2w.apply_default(0.0);
        $values.wdrout.apply_default(0.0);
        $values.wdsub.apply_default(0.0);
        $values.wvth0.apply_default(0.0);
        $values.wua.apply_default(0.0);
        $values.wua1.apply_default(0.0);
        $values.wub.apply_default(0.0);
        $values.wub1.apply_default(0.0);
        $values.wuc.apply_default(0.0);
        $values.wuc1.apply_default(0.0);
        $values.wu0.apply_default(0.0);
        $values.wute.apply_default(0.0);
        $values.wvoff.apply_default(0.0);
        $values.wdelta.apply_default(0.0);
        $values.wrdsw.apply_default(0.0);
        $values.wprwb.apply_default(0.0);
        $values.wprwg.apply_default(0.0);
        $values.wprt.apply_default(0.0);
        $values.weta0.apply_default(0.0);
        $values.wetab.apply_default(0.0);
        $values.wpclm.apply_default(0.0);
        $values.wpdiblc1.apply_default(0.0);
        $values.wpdiblc2.apply_default(0.0);
        $values.wpdiblcb.apply_default(0.0);
        $values.wpscbe1.apply_default(0.0);
        $values.wpscbe2.apply_default(0.0);
        $values.wpvag.apply_default(0.0);
        $values.wwr.apply_default(0.0);
        $values.wdwg.apply_default(0.0);
        $values.wdwb.apply_default(0.0);
        $values.wb0.apply_default(0.0);
        $values.wb1.apply_default(0.0);
        $values.walpha0.apply_default(0.0);
        $values.walpha1.apply_default(0.0);
        $values.wbeta0.apply_default(0.0);
        $values.wvfb.apply_default(0.0);
        $values.welm.apply_default(0.0);
        $values.wcgsl.apply_default(0.0);
        $values.wcgdl.apply_default(0.0);
        $values.wckappa.apply_default(0.0);
        $values.wcf.apply_default(0.0);
        $values.wclc.apply_default(0.0);
        $values.wcle.apply_default(0.0);
        $values.wvfbcv.apply_default(0.0);
        $values.wacde.apply_default(0.0);
        $values.wmoin.apply_default(0.0);
        $values.wnoff.apply_default(0.0);
        $values.wvoffcv.apply_default(0.0);
        $values.pcdsc.apply_default(0.0);
        $values.pcdscb.apply_default(0.0);
        $values.pcdscd.apply_default(0.0);
        $values.pcit.apply_default(0.0);
        $values.pnfactor.apply_default(0.0);
        $values.pxj.apply_default(0.0);
        $values.pvsat.apply_default(0.0);
        $values.pat.apply_default(0.0);
        $values.pa0.apply_default(0.0);
        $values.pags.apply_default(0.0);
        $values.pa1.apply_default(0.0);
        $values.pa2.apply_default(0.0);
        $values.pketa.apply_default(0.0);
        $values.pnsub.apply_default(0.0);
        $values.pnch.apply_default(0.0);
        $values.pngate.apply_default(0.0);
        $values.pvbm.apply_default(0.0);
        $values.pxt.apply_default(0.0);
        $values.pkt1.apply_default(0.0);
        $values.pkt1l.apply_default(0.0);
        $values.pkt2.apply_default(0.0);
        $values.pk3.apply_default(0.0);
        $values.pk3b.apply_default(0.0);
        $values.pw0.apply_default(0.0);
        $values.pnlx.apply_default(0.0);
        $values.pdvt0.apply_default(0.0);
        $values.pdvt1.apply_default(0.0);
        $values.pdvt2.apply_default(0.0);
        $values.pdvt0w.apply_default(0.0);
        $values.pdvt1w.apply_default(0.0);
        $values.pdvt2w.apply_default(0.0);
        $values.pdrout.apply_default(0.0);
        $values.pdsub.apply_default(0.0);
        $values.pvth0.apply_default(0.0);
        $values.pua.apply_default(0.0);
        $values.pua1.apply_default(0.0);
        $values.r#pub.apply_default(0.0);
        $values.pub1.apply_default(0.0);
        $values.puc.apply_default(0.0);
        $values.puc1.apply_default(0.0);
        $values.pu0.apply_default(0.0);
        $values.pute.apply_default(0.0);
        $values.pvoff.apply_default(0.0);
        $values.pdelta.apply_default(0.0);
        $values.prdsw.apply_default(0.0);
        $values.pprwb.apply_default(0.0);
        $values.pprwg.apply_default(0.0);
        $values.pprt.apply_default(0.0);
        $values.peta0.apply_default(0.0);
        $values.petab.apply_default(0.0);
        $values.ppclm.apply_default(0.0);
        $values.ppdiblc1.apply_default(0.0);
        $values.ppdiblc2.apply_default(0.0);
        $values.ppdiblcb.apply_default(0.0);
        $values.ppscbe1.apply_default(0.0);
        $values.ppscbe2.apply_default(0.0);
        $values.ppvag.apply_default(0.0);
        $values.pwr.apply_default(0.0);
        $values.pdwg.apply_default(0.0);
        $values.pdwb.apply_default(0.0);
        $values.pb0.apply_default(0.0);
        $values.pb1.apply_default(0.0);
        $values.palpha0.apply_default(0.0);
        $values.palpha1.apply_default(0.0);
        $values.pbeta0.apply_default(0.0);
        $values.pvfb.apply_default(0.0);
        $values.pelm.apply_default(0.0);
        $values.pcgsl.apply_default(0.0);
        $values.pcgdl.apply_default(0.0);
        $values.pckappa.apply_default(0.0);
        $values.pcf.apply_default(0.0);
        $values.pclc.apply_default(0.0);
        $values.pcle.apply_default(0.0);
        $values.pvfbcv.apply_default(0.0);
        $values.pacde.apply_default(0.0);
        $values.pmoin.apply_default(0.0);
        $values.pnoff.apply_default(0.0);
        $values.pvoffcv.apply_default(0.0);
        $values.lintnoi.apply_default(0.0);
        $values.lint.apply_default(0.0);
        $values.ll.apply_default(0.0);
        $values.lln.apply_default(1.0);
        $values.lw.apply_default(0.0);
        $values.lwn.apply_default(1.0);
        $values.lwl.apply_default(0.0);
        $values.lmin.apply_default(0.0);
        $values.lmax.apply_default(1.0);
        $values.wint.apply_default(0.0);
        $values.wl.apply_default(0.0);
        $values.wln.apply_default(1.0);
        $values.ww.apply_default(0.0);
        $values.wwn.apply_default(1.0);
        $values.wwl.apply_default(0.0);
        $values.wmin.apply_default(0.0);
        $values.wmax.apply_default(1.0);
        $values.xl.apply_default(0.0);
        $values.xw.apply_default(0.0);
        $values.xpart.apply_default(0.0);
        $values.rsh.apply_default(0.0);
        $values.cj.apply_default(0.0005);
        $values.cjsw.apply_default(5e-10);
        $values.js.apply_default(0.0001);
        $values.jsw.apply_default(0.0);
        $values.pb.apply_default(1.0);
        $values.pbsw.apply_default(1.0);
        $values.mj.apply_default(0.5);
        $values.mjsw.apply_default(0.33);
        $values.nj.apply_default(1.0);
        $values.xti.apply_default(3.0);
        $values.em.apply_default(41_000_000.0);
        $values.ef.apply_default(1.0);
        $values.af.apply_default(1.0);
        $values.kf.apply_default(0.0);
        $values.vgs_max.apply_default(1e99);
        $values.vgd_max.apply_default(1e99);
        $values.vgb_max.apply_default(1e99);
        $values.vds_max.apply_default(1e99);
        $values.vbs_max.apply_default(1e99);
        $values.vbd_max.apply_default(1e99);
        $values.vgsr_max.apply_default(1e99);
        $values.vgdr_max.apply_default(1e99);
        $values.vgbr_max.apply_default(1e99);
        $values.vbsr_max.apply_default(1e99);
        $values.vbdr_max.apply_default(1e99);
    };
}
pub(super) use apply_literal_defaults;

// Canonical typed numeric storage, derived from the same IFparm table.
#[rustfmt::skip]
macro_rules! model_fields {
    ($define:ident) => {
        $define! {
            real {
                ("tox", tox),
                ("toxm", toxm),
                ("cdsc", cdsc),
                ("cdscb", cdscb),
                ("cdscd", cdscd),
                ("cit", cit),
                ("nfactor", nfactor),
                ("xj", xj),
                ("vsat", vsat),
                ("at", at),
                ("a0", a0),
                ("ags", ags),
                ("a1", a1),
                ("a2", a2),
                ("keta", keta),
                ("nsub", nsub),
                ("nch", nch),
                ("ngate", ngate),
                ("gamma1", gamma1),
                ("gamma2", gamma2),
                ("vbx", vbx),
                ("vbm", vbm),
                ("xt", xt),
                ("k1", k1),
                ("kt1", kt1),
                ("kt1l", kt1l),
                ("kt2", kt2),
                ("k2", k2),
                ("k3", k3),
                ("k3b", k3b),
                ("w0", w0),
                ("nlx", nlx),
                ("dvt0", dvt0),
                ("dvt1", dvt1),
                ("dvt2", dvt2),
                ("dvt0w", dvt0w),
                ("dvt1w", dvt1w),
                ("dvt2w", dvt2w),
                ("drout", drout),
                ("dsub", dsub),
                ("vth0", vth0),
                ("ua", ua),
                ("ua1", ua1),
                ("ub", ub),
                ("ub1", ub1),
                ("uc", uc),
                ("uc1", uc1),
                ("u0", u0),
                ("ute", ute),
                ("voff", voff),
                ("tnom", tnom),
                ("cgso", cgso),
                ("cgdo", cgdo),
                ("cgbo", cgbo),
                ("xpart", xpart),
                ("elm", elm),
                ("delta", delta),
                ("rsh", rsh),
                ("rdsw", rdsw),
                ("prwg", prwg),
                ("prwb", prwb),
                ("prt", prt),
                ("eta0", eta0),
                ("etab", etab),
                ("pclm", pclm),
                ("pdiblc1", pdiblc1),
                ("pdiblc2", pdiblc2),
                ("pdiblcb", pdiblcb),
                ("pscbe1", pscbe1),
                ("pscbe2", pscbe2),
                ("pvag", pvag),
                ("js", js),
                ("jsw", jsw),
                ("pb", pb),
                ("nj", nj),
                ("xti", xti),
                ("mj", mj),
                ("pbsw", pbsw),
                ("mjsw", mjsw),
                ("pbswg", pbswg),
                ("mjswg", mjswg),
                ("cj", cj),
                ("vfbcv", vfbcv),
                ("vfb", vfb),
                ("cjsw", cjsw),
                ("cjswg", cjswg),
                ("tpb", tpb),
                ("tcj", tcj),
                ("tpbsw", tpbsw),
                ("tcjsw", tcjsw),
                ("tpbswg", tpbswg),
                ("tcjswg", tcjswg),
                ("acde", acde),
                ("moin", moin),
                ("noff", noff),
                ("voffcv", voffcv),
                ("lintnoi", lintnoi),
                ("lint", lint),
                ("ll", ll),
                ("llc", llc),
                ("lln", lln),
                ("lw", lw),
                ("lwc", lwc),
                ("lwn", lwn),
                ("lwl", lwl),
                ("lwlc", lwlc),
                ("lmin", lmin),
                ("lmax", lmax),
                ("xl", xl),
                ("xw", xw),
                ("wr", wr),
                ("wint", wint),
                ("dwg", dwg),
                ("dwb", dwb),
                ("wl", wl),
                ("wlc", wlc),
                ("wln", wln),
                ("ww", ww),
                ("wwc", wwc),
                ("wwn", wwn),
                ("wwl", wwl),
                ("wwlc", wwlc),
                ("wmin", wmin),
                ("wmax", wmax),
                ("b0", b0),
                ("b1", b1),
                ("cgsl", cgsl),
                ("cgdl", cgdl),
                ("ckappa", ckappa),
                ("cf", cf),
                ("clc", clc),
                ("cle", cle),
                ("dwc", dwc),
                ("dlc", dlc),
                ("hdif", hdif),
                ("ldif", ldif),
                ("ld", ld),
                ("rd", rd),
                ("rs", rs),
                ("rdc", rdc),
                ("rsc", rsc),
                ("wmlt", wmlt),
                ("alpha0", alpha0),
                ("alpha1", alpha1),
                ("beta0", beta0),
                ("ijth", ijth),
                ("lcdsc", lcdsc),
                ("lcdscb", lcdscb),
                ("lcdscd", lcdscd),
                ("lcit", lcit),
                ("lnfactor", lnfactor),
                ("lxj", lxj),
                ("lvsat", lvsat),
                ("lat", lat),
                ("la0", la0),
                ("lags", lags),
                ("la1", la1),
                ("la2", la2),
                ("lketa", lketa),
                ("lnsub", lnsub),
                ("lnch", lnch),
                ("lngate", lngate),
                ("lgamma1", lgamma1),
                ("lgamma2", lgamma2),
                ("lvbx", lvbx),
                ("lvbm", lvbm),
                ("lxt", lxt),
                ("lk1", lk1),
                ("lkt1", lkt1),
                ("lkt1l", lkt1l),
                ("lkt2", lkt2),
                ("lk2", lk2),
                ("lk3", lk3),
                ("lk3b", lk3b),
                ("lw0", lw0),
                ("lnlx", lnlx),
                ("ldvt0", ldvt0),
                ("ldvt1", ldvt1),
                ("ldvt2", ldvt2),
                ("ldvt0w", ldvt0w),
                ("ldvt1w", ldvt1w),
                ("ldvt2w", ldvt2w),
                ("ldrout", ldrout),
                ("ldsub", ldsub),
                ("lvth0", lvth0),
                ("lua", lua),
                ("lua1", lua1),
                ("lub", lub),
                ("lub1", lub1),
                ("luc", luc),
                ("luc1", luc1),
                ("lu0", lu0),
                ("lute", lute),
                ("lvoff", lvoff),
                ("lelm", lelm),
                ("ldelta", ldelta),
                ("lrdsw", lrdsw),
                ("lprwg", lprwg),
                ("lprwb", lprwb),
                ("lprt", lprt),
                ("leta0", leta0),
                ("letab", letab),
                ("lpclm", lpclm),
                ("lpdiblc1", lpdiblc1),
                ("lpdiblc2", lpdiblc2),
                ("lpdiblcb", lpdiblcb),
                ("lpscbe1", lpscbe1),
                ("lpscbe2", lpscbe2),
                ("lpvag", lpvag),
                ("lwr", lwr),
                ("ldwg", ldwg),
                ("ldwb", ldwb),
                ("lb0", lb0),
                ("lb1", lb1),
                ("lcgsl", lcgsl),
                ("lcgdl", lcgdl),
                ("lckappa", lckappa),
                ("lcf", lcf),
                ("lclc", lclc),
                ("lcle", lcle),
                ("lalpha0", lalpha0),
                ("lalpha1", lalpha1),
                ("lbeta0", lbeta0),
                ("lvfbcv", lvfbcv),
                ("lvfb", lvfb),
                ("lacde", lacde),
                ("lmoin", lmoin),
                ("lnoff", lnoff),
                ("lvoffcv", lvoffcv),
                ("wcdsc", wcdsc),
                ("wcdscb", wcdscb),
                ("wcdscd", wcdscd),
                ("wcit", wcit),
                ("wnfactor", wnfactor),
                ("wxj", wxj),
                ("wvsat", wvsat),
                ("wat", wat),
                ("wa0", wa0),
                ("wags", wags),
                ("wa1", wa1),
                ("wa2", wa2),
                ("wketa", wketa),
                ("wnsub", wnsub),
                ("wnch", wnch),
                ("wngate", wngate),
                ("wgamma1", wgamma1),
                ("wgamma2", wgamma2),
                ("wvbx", wvbx),
                ("wvbm", wvbm),
                ("wxt", wxt),
                ("wk1", wk1),
                ("wkt1", wkt1),
                ("wkt1l", wkt1l),
                ("wkt2", wkt2),
                ("wk2", wk2),
                ("wk3", wk3),
                ("wk3b", wk3b),
                ("ww0", ww0),
                ("wnlx", wnlx),
                ("wdvt0", wdvt0),
                ("wdvt1", wdvt1),
                ("wdvt2", wdvt2),
                ("wdvt0w", wdvt0w),
                ("wdvt1w", wdvt1w),
                ("wdvt2w", wdvt2w),
                ("wdrout", wdrout),
                ("wdsub", wdsub),
                ("wvth0", wvth0),
                ("wua", wua),
                ("wua1", wua1),
                ("wub", wub),
                ("wub1", wub1),
                ("wuc", wuc),
                ("wuc1", wuc1),
                ("wu0", wu0),
                ("wute", wute),
                ("wvoff", wvoff),
                ("welm", welm),
                ("wdelta", wdelta),
                ("wrdsw", wrdsw),
                ("wprwg", wprwg),
                ("wprwb", wprwb),
                ("wprt", wprt),
                ("weta0", weta0),
                ("wetab", wetab),
                ("wpclm", wpclm),
                ("wpdiblc1", wpdiblc1),
                ("wpdiblc2", wpdiblc2),
                ("wpdiblcb", wpdiblcb),
                ("wpscbe1", wpscbe1),
                ("wpscbe2", wpscbe2),
                ("wpvag", wpvag),
                ("wwr", wwr),
                ("wdwg", wdwg),
                ("wdwb", wdwb),
                ("wb0", wb0),
                ("wb1", wb1),
                ("wcgsl", wcgsl),
                ("wcgdl", wcgdl),
                ("wckappa", wckappa),
                ("wcf", wcf),
                ("wclc", wclc),
                ("wcle", wcle),
                ("walpha0", walpha0),
                ("walpha1", walpha1),
                ("wbeta0", wbeta0),
                ("wvfbcv", wvfbcv),
                ("wvfb", wvfb),
                ("wacde", wacde),
                ("wmoin", wmoin),
                ("wnoff", wnoff),
                ("wvoffcv", wvoffcv),
                ("pcdsc", pcdsc),
                ("pcdscb", pcdscb),
                ("pcdscd", pcdscd),
                ("pcit", pcit),
                ("pnfactor", pnfactor),
                ("pxj", pxj),
                ("pvsat", pvsat),
                ("pat", pat),
                ("pa0", pa0),
                ("pags", pags),
                ("pa1", pa1),
                ("pa2", pa2),
                ("pketa", pketa),
                ("pnsub", pnsub),
                ("pnch", pnch),
                ("pngate", pngate),
                ("pgamma1", pgamma1),
                ("pgamma2", pgamma2),
                ("pvbx", pvbx),
                ("pvbm", pvbm),
                ("pxt", pxt),
                ("pk1", pk1),
                ("pkt1", pkt1),
                ("pkt1l", pkt1l),
                ("pkt2", pkt2),
                ("pk2", pk2),
                ("pk3", pk3),
                ("pk3b", pk3b),
                ("pw0", pw0),
                ("pnlx", pnlx),
                ("pdvt0", pdvt0),
                ("pdvt1", pdvt1),
                ("pdvt2", pdvt2),
                ("pdvt0w", pdvt0w),
                ("pdvt1w", pdvt1w),
                ("pdvt2w", pdvt2w),
                ("pdrout", pdrout),
                ("pdsub", pdsub),
                ("pvth0", pvth0),
                ("pua", pua),
                ("pua1", pua1),
                ("pub", r#pub),
                ("pub1", pub1),
                ("puc", puc),
                ("puc1", puc1),
                ("pu0", pu0),
                ("pute", pute),
                ("pvoff", pvoff),
                ("pelm", pelm),
                ("pdelta", pdelta),
                ("prdsw", prdsw),
                ("pprwg", pprwg),
                ("pprwb", pprwb),
                ("pprt", pprt),
                ("peta0", peta0),
                ("petab", petab),
                ("ppclm", ppclm),
                ("ppdiblc1", ppdiblc1),
                ("ppdiblc2", ppdiblc2),
                ("ppdiblcb", ppdiblcb),
                ("ppscbe1", ppscbe1),
                ("ppscbe2", ppscbe2),
                ("ppvag", ppvag),
                ("pwr", pwr),
                ("pdwg", pdwg),
                ("pdwb", pdwb),
                ("pb0", pb0),
                ("pb1", pb1),
                ("pcgsl", pcgsl),
                ("pcgdl", pcgdl),
                ("pckappa", pckappa),
                ("pcf", pcf),
                ("pclc", pclc),
                ("pcle", pcle),
                ("palpha0", palpha0),
                ("palpha1", palpha1),
                ("pbeta0", pbeta0),
                ("pvfbcv", pvfbcv),
                ("pvfb", pvfb),
                ("pacde", pacde),
                ("pmoin", pmoin),
                ("pnoff", pnoff),
                ("pvoffcv", pvoffcv),
                ("noia", noia),
                ("noib", noib),
                ("noic", noic),
                ("em", em),
                ("ef", ef),
                ("af", af),
                ("kf", kf),
                ("vgs_max", vgs_max),
                ("vgd_max", vgd_max),
                ("vgb_max", vgb_max),
                ("vds_max", vds_max),
                ("vbs_max", vbs_max),
                ("vbd_max", vbd_max),
                ("vgsr_max", vgsr_max),
                ("vgdr_max", vgdr_max),
                ("vgbr_max", vgbr_max),
                ("vbsr_max", vbsr_max),
                ("vbdr_max", vbdr_max),
            }
            integer {
                ("capmod", capmod),
                ("mobmod", mobmod),
                ("noimod", noimod),
                ("nqsmod", nqsmod),
                ("acnqsmod", acnqsmod),
                ("acm", acm),
                ("calcacm", calcacm),
                ("paramchk", paramchk),
                ("binunit", binunit),
            }
        }
    };
}
pub(super) use model_fields;
