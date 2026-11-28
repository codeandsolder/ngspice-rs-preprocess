"""Extract the value-only CODEP transport kernel from native HiSIM-HV2."""
from itertools import product
from pathlib import Path
import math,re,subprocess,sys,tempfile
REVISION='972dbf45db1e47d18bc4fda5d8bb829aff2d0e05'

def program(root):
    def source(name):
        return subprocess.check_output(['git','-C',str(root),'show',f'{REVISION}:src/spicelib/devices/hisimhv2/{name}'],text=True)
    dep=source('hsmhv2eval_dep.h')
    eval_c=source('hsmhv2eval.c')
    macros=eval_c[eval_c.index('#define Fn_CP('):eval_c.index('/*---------------------------------------------------*',eval_c.index('#define Fn_SL_CP('))]
    def values(chunk):
        chunk = re.sub(r'(?m)^\s*\w+_d\w*\s*(?:/=|=)[^;]*;', '', chunk)
        return re.sub(r'(?m)^\s*T3\s*=\s*here->HSMHV2_(?:dep)?mphn1[^;]*;', '', chunk)
    surface=dep[dep.index('       Ps0 = phi_s0_DEP ;'):dep.index('     //*  resistor region current.')]
    neutral_start=dep.index('       Qiu = - Qn_res0 ;')
    neutral_start=dep.rfind('       T0 = here->HSMHV2_ninvd ;',0,neutral_start)
    neutral_end=dep.index('       // Vdseff //',neutral_start)
    neutral=dep[neutral_start:neutral_end]
    charges=dep[dep.index('       Qbu = - 0.5 * (Q_sub0'):dep.index('       Qdrat_dVgs =',dep.index('       Qbu = - 0.5 * (Q_sub0'))]
    surface=values(surface).replace('       Mu = Muun * T5 ;','       Mu = Muun * T5 ;\n       surface_mu0=Muun;')
    neutral=values(neutral)
    return f'''#include <math.h>
#include <stdio.h>
#include <float.h>
#define C_QE 1.6021918e-19
#define C_m2cm_p2 1e4
#define Fn_Pow pow
{macros}
struct Instance {{double HSMHV2_ninv_o_esi,HSMHV2_ninvd,HSMHV2_muesr,HSMHV2_mphn0,HSMHV2_depmphn0,HSMHV2_depvmax,HSMHV2_weff_nf;}};
struct Model {{double HSMHV2_mueph0,HSMHV2_bb,HSMHV2_depmueph0,HSMHV2_depmue0,HSMHV2_depmue1,HSMHV2_depmueback0,HSMHV2_depmueback1,HSMHV2_depbb;}};
struct Binned {{double HSMHV2_muecb0,HSMHV2_muecb1,HSMHV2_muesr1;}};
int main(void) {{
 double phi_s0_DEP,phi_sL_DEP,Q_s0,Q_sL,Q_n0_cur,Q_nL_cur,Qn_res0,Qn_bac0,Qn_delta,Vdseff0,beta,Leff,Vmax;
 double Q_n0,Q_nL,Q_s0_dep,Q_sL_dep,Q_b0_dep,Q_bL_dep,Q_sub0,Q_subL,Q_sub0_dep,Q_subL_dep;
 struct Instance inst;struct Instance *here=&inst;
 struct Model modelval;struct Model *model=&modelval;
 struct Binned binned;struct Binned *pParam=&binned;
 double *inputs[]={{&phi_s0_DEP,&phi_sL_DEP,&Q_s0,&Q_sL,&Q_n0_cur,&Q_nL_cur,&Qn_res0,&Qn_bac0,&Qn_delta,&Vdseff0,&beta,&Leff,&Vmax,
 &Q_n0,&Q_nL,&Q_s0_dep,&Q_sL_dep,&Q_b0_dep,&Q_bL_dep,&Q_sub0,&Q_subL,&Q_sub0_dep,&Q_subL_dep,
 &inst.HSMHV2_ninv_o_esi,&inst.HSMHV2_ninvd,&inst.HSMHV2_muesr,&inst.HSMHV2_mphn0,&inst.HSMHV2_depmphn0,&inst.HSMHV2_depvmax,&inst.HSMHV2_weff_nf,
 &modelval.HSMHV2_mueph0,&modelval.HSMHV2_bb,&modelval.HSMHV2_depmueph0,&modelval.HSMHV2_depmue0,&modelval.HSMHV2_depmue1,&modelval.HSMHV2_depmueback0,&modelval.HSMHV2_depmueback1,&modelval.HSMHV2_depbb,
 &binned.HSMHV2_muecb0,&binned.HSMHV2_muecb1,&binned.HSMHV2_muesr1}};
 while(1) {{
 for (size_t i=0;i<sizeof(inputs)/sizeof(inputs[0]);i++) if(scanf("%lf",inputs[i])!=1)return 0;
 double epsm10=10*DBL_EPSILON,small=1e-25,beta_inv=1/beta;
 double Ps0,Psl,Pds,Idd=0,Idd_drift=0,Idd_diffu=0,Qn_drift=0,Qiu,Lch,T0,T1,T2,T3,T4,T5,T6,T7,T8,T9,T10,TMF0,TMF1,TMF2;
 double Eeff,Rns,Muun,TY,Ey,Em,Ey_suf,Mu,Mu_res,Mu_bac,Edri,Ids_res,Ids_bac,betaWL,Ids0,Qbu,Qdrat,surface_mu0;
 {surface}
 {neutral}
 {values(charges)}
 printf("%.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g\\n",Pds,Idd,surface_mu0,Mu,Mu_res,Mu_bac,betaWL*Idd*Mu,Ids_res,Ids_bac,Qbu,Qiu);
 }}
}}
'''

def main():
    root,output=map(Path,sys.argv[1:]);cases=[]
    states=[(-.008,-.005,-.006,-.003,-.002,-.001),(.003,.004,-1e-10,-2e-11,-.0002,-.00002),(-.0001,.0004,-.0001,-1e-8,-1e-9,-.0001),(-1e-7,-3e-8,-1e-7,-3e-8,-.005,-.00001)]
    for (qs,qd,cs,cd,qr,qb),delta,vd,beta,bb,dbb,length in product(states,(-1e-10,0.,1e-8,.05,.5,2.),(0.,.01,.5,4.),(27.,44.),(1.,2.,1.4),(1.,2.,1.7),(.5e-6,5e-6)):
        cases.append((.2,.2+delta,qs,qd,cs,cd,qr,qb,.0004,vd,beta,length,1e5,min(qs,0),min(qd,0),.0001,.0002,.001,.002,.00003,.00004,-.001,-.002,4e7,.2,2.,1e-9,1e-3,8e4,10e-6,1.3,bb,1.1,200.,10.,300.,5.,dbb,400.,20.,1e15))
    with tempfile.TemporaryDirectory(prefix='hisim-dep-transport-') as tmp:
        c,exe=Path(tmp)/'oracle.c',Path(tmp)/'oracle';c.write_text(program(root))
        subprocess.run(['sccache','cc','-std=c11','-O2',str(c),'-lm','-o',str(exe)],check=True)
        rows=subprocess.run([str(exe)],input=''.join(' '.join(map(str,row))+'\n' for row in cases),text=True,capture_output=True,check=True).stdout.splitlines();assert len(rows)==len(cases)
        with output.open('w') as f:
            f.write('# 41 native transport inputs; Pds Idd Muun_surface Mu_surface Mu_res Mu_back I_surface I_res I_back Qbu Qiu\n')
            for case,row in zip(cases,rows):
                assert all(math.isfinite(float(x)) for x in row.split()),(case,row)
                f.write(' '.join(map(str,case))+' '+row+'\n')
        print(len(cases))
if __name__=='__main__':main()
